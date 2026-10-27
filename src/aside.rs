//! Asking Copilot something on the side.
//!
//! CST has a few questions for a model that are not any session's business: which emoji
//! suits a topic, whether two agents in a thread have stopped getting anywhere, what a
//! session's conversation has been about when someone asks from the phone. Each used to
//! be a `copilot -p` call, which starts the whole CLI for one line, saves a session
//! named after the prompt, and takes flags some default models refuse. This module keeps
//! one Copilot running in server mode instead and asks through its SDK: an answer in a
//! few seconds, a session made for the question and deleted after it, and an existing
//! session's conversation copied in when the question is about one.
//!
//! The posture is the judge's, for every question: no tools at all, no MCP servers, no
//! configuration discovered from the working directory, every permission denied. Some
//! of what the model reads was written by outsiders — a thread's comments, a session's
//! tool output — and a model that can only answer in words cannot be talked into
//! anything else.
//!
//! The server starts on the first question and stops after a quiet while, so a CST that
//! never asks never runs it. It is a child speaking over pipes, and the CLI exits when
//! its pipe closes, so a CST that dies takes it along.

use std::fmt;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use github_copilot_sdk::rpc::SessionsForkRequest;
use github_copilot_sdk::types::{MessageOptions, ResumeSessionConfig, SessionId};
use github_copilot_sdk::{CliProgram, Client, ClientOptions, SessionConfig};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

/// What every session made here is called. The session list hides sessions of this
/// name: a question's session exists for seconds, and a copy of a conversation is
/// otherwise indistinguishable from the conversation itself.
pub const SESSION_NAME: &str = "CST aside (temporary)";

/// How long the server is kept after its last question. Starting it costs a few seconds
/// of CLI start-up, so questions that come in bursts share one start.
pub const IDLE: Duration = Duration::from_secs(10 * 60);

/// How long starting the server may take before the question is given up.
const START_TIMEOUT: Duration = Duration::from_secs(60);

/// One question, and what the model may see while answering it.
#[derive(Debug, Clone)]
pub struct Question {
    pub prompt: String,
    /// A session whose conversation the model should know: a copy of it is made and the
    /// question asked in the copy, so nothing of the original changes.
    pub about: Option<String>,
    /// Where a question without a session runs. Callers pass an empty scratch folder.
    pub working_dir: Option<PathBuf>,
    pub timeout: Duration,
}

/// Why a question went unanswered. Plain sentences, because some reach the phone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    NoCopilot,
    Start(String),
    /// The session asked about has no conversation to copy yet.
    NoConversation,
    Fork(String),
    Session(String),
    Timeout,
    /// The turn ended without a message.
    Empty,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::NoCopilot => write!(f, "Copilot is not installed"),
            Failure::Start(why) => write!(f, "Copilot could not be started: {why}"),
            Failure::NoConversation => write!(f, "that session has no conversation yet"),
            Failure::Fork(why) => write!(f, "the conversation could not be copied: {why}"),
            Failure::Session(why) => write!(f, "Copilot did not answer: {why}"),
            Failure::Timeout => write!(f, "Copilot took too long to answer"),
            Failure::Empty => write!(f, "Copilot finished without saying anything"),
        }
    }
}

pub type Answer = Result<String, Failure>;

struct Job {
    question: Question,
    reply: std::sync::mpsc::Sender<Answer>,
}

static SERVER: OnceLock<Mutex<Option<UnboundedSender<Job>>>> = OnceLock::new();

fn server() -> &'static Mutex<Option<UnboundedSender<Job>>> {
    SERVER.get_or_init(|| Mutex::new(None))
}

/// Ask, and wait for the answer. Bounded by the question's timeout plus a server start.
pub fn ask(question: Question) -> Answer {
    ask_later(question).recv().unwrap_or_else(|_| {
        Err(Failure::Start(
            "the Copilot server thread ended".to_string(),
        ))
    })
}

/// Ask, and read the answer later from the receiver. Starts the server if it is not
/// running; questions run concurrently, each in its own session.
pub fn ask_later(question: Question) -> std::sync::mpsc::Receiver<Answer> {
    let (reply, receiver) = std::sync::mpsc::channel();
    let mut job = Job { question, reply };
    let mut slot = server()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(sender) = slot.as_ref() {
        match sender.send(job) {
            Ok(()) => return receiver,
            // The server retired between our look and our send; start another.
            Err(returned) => job = returned.0,
        }
    }
    let (sender, jobs) = mpsc::unbounded_channel();
    let _ = sender.send(job);
    *slot = Some(sender);
    let spawned = std::thread::Builder::new()
        .name("copilot-aside".to_string())
        .spawn(move || serve(jobs));
    if spawned.is_err() {
        // Dropping the queue drops the job's reply sender, so the caller's wait ends.
        *slot = None;
    }
    receiver
}

/// Stop the server if it is running. Questions in flight end unanswered.
pub fn shutdown() {
    let mut slot = server()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *slot = None;
}

/// The server retired: the next question starts a fresh one.
fn retire() {
    shutdown();
}

fn serve(mut jobs: UnboundedReceiver<Job>) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            retire();
            fail_all(&mut jobs, &Failure::Start(error.to_string()));
            return;
        }
    };
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, async move {
        let client = match start_client().await {
            Ok(client) => client,
            Err(failure) => {
                retire();
                fail_all(&mut jobs, &failure);
                return;
            }
        };
        let mut running: Vec<tokio::task::JoinHandle<()>> = Vec::new();
        loop {
            running.retain(|task| !task.is_finished());
            match tokio::time::timeout(IDLE, jobs.recv()).await {
                Ok(Some(job)) => running.push(spawn(&client, job)),
                // The sender is gone: CST is shutting down.
                Ok(None) => break,
                Err(_) => {
                    retire();
                    // A question sent in the moment before retiring is still ours.
                    while let Ok(job) = jobs.try_recv() {
                        running.push(spawn(&client, job));
                    }
                    break;
                }
            }
        }
        for task in running {
            let _ = task.await;
        }
        let _ = client.stop().await;
    });
}

fn spawn(client: &Client, job: Job) -> tokio::task::JoinHandle<()> {
    let client = client.clone();
    tokio::task::spawn_local(async move {
        let answer = answer(&client, job.question).await;
        let _ = job.reply.send(answer);
    })
}

fn fail_all(jobs: &mut UnboundedReceiver<Job>, failure: &Failure) {
    jobs.close();
    while let Ok(job) = jobs.try_recv() {
        let _ = job.reply.send(Err(failure.clone()));
    }
}

async fn start_client() -> Result<Client, Failure> {
    let probe = crate::session::manager::copilot_probe().ok_or(Failure::NoCopilot)?;
    let options =
        ClientOptions::new().with_program(CliProgram::Path(PathBuf::from(&probe.program)));
    tokio::time::timeout(START_TIMEOUT, Client::start(options))
        .await
        .map_err(|_| {
            Failure::Start("the Copilot server did not answer within a minute".to_string())
        })?
        .map_err(|error| Failure::Start(error.to_string()))
}

/// One question in one session, which is deleted whatever happened.
async fn answer(client: &Client, question: Question) -> Answer {
    let (id, session) = match &question.about {
        Some(source) => {
            let forked = client
                .rpc()
                .sessions()
                .fork(SessionsForkRequest {
                    session_id: SessionId::new(source.clone()),
                    name: Some(SESSION_NAME.to_string()),
                    ..Default::default()
                })
                .await
                .map_err(|error| fork_failure(&error.to_string()))?;
            let id = forked.session_id;
            let mut config = ResumeSessionConfig::new(id.clone()).deny_all_permissions();
            config.available_tools = Some(Vec::new());
            config.enable_config_discovery = Some(false);
            (id, client.resume_session(config).await)
        }
        None => {
            let id = SessionId::new(crate::session::manager::new_session_id());
            let mut config = SessionConfig::default()
                .with_session_id(id.clone())
                .with_name(SESSION_NAME)
                .deny_all_permissions()
                .with_available_tools(Vec::<String>::new())
                .with_enable_config_discovery(false);
            config.working_directory = question.working_dir.clone();
            (id, client.create_session(config).await)
        }
    };
    let result = match session {
        Ok(session) => {
            let outcome = session
                .send_and_wait(
                    MessageOptions::new(question.prompt).with_wait_timeout(question.timeout),
                )
                .await;
            let answer = match outcome {
                Ok(Some(event)) => event
                    .data
                    .get("content")
                    .and_then(|content| content.as_str())
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(str::to_string)
                    .ok_or(Failure::Empty),
                Ok(None) => Err(Failure::Empty),
                Err(error) if error.to_string().contains("timed out") => {
                    let _ = session.abort().await;
                    Err(Failure::Timeout)
                }
                Err(error) => Err(Failure::Session(error.to_string())),
            };
            let _ = session.disconnect().await;
            answer
        }
        Err(error) => Err(Failure::Session(error.to_string())),
    };
    // Whatever happened, the session made for this question does not stay.
    let _ = client.delete_session(&id).await;
    result
}

/// The CLI's one expected refusal, told apart from the rest so the phone hears "no
/// conversation yet" rather than an RPC error.
fn fork_failure(message: &str) -> Failure {
    if message.contains("no persisted or in-memory events") {
        Failure::NoConversation
    } else {
        Failure::Fork(message.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Against the Copilot on this machine: `cargo test live_copilot -- --ignored`.
    /// Proves the resolved program name starts in server mode, a question is answered,
    /// and the session made for it is gone afterwards.
    #[test]
    #[ignore = "starts the real Copilot CLI"]
    fn live_copilot_answers_a_side_question_and_leaves_no_session_behind() {
        let scratch = tempfile::tempdir().unwrap();
        let before: std::collections::HashSet<_> = std::fs::read_dir(
            crate::session::loader::copilot_home().join("session-state"),
        )
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name())
        .collect();
        let answer = ask(Question {
            prompt: "Reply with the single word pong.".to_string(),
            about: None,
            working_dir: Some(scratch.path().to_path_buf()),
            timeout: Duration::from_secs(60),
        });
        shutdown();
        let after: std::collections::HashSet<_> = std::fs::read_dir(
            crate::session::loader::copilot_home().join("session-state"),
        )
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name())
        .collect();
        assert_eq!(answer.as_deref().map(|a| a.to_lowercase()), Ok("pong".to_string()));
        assert_eq!(after.difference(&before).count(), 0, "a session was left behind");
    }

    #[test]
    fn a_fork_refused_for_want_of_history_is_reported_as_no_conversation_yet() {
        // A session opened a moment ago has nothing to copy; that is not an error to
        // alarm anyone with, and the phone should hear it in words.
        let refusal = "RPC error -32603: Request sessions.fork failed with message: Cannot fork \
                       session abc: no persisted or in-memory events found";
        assert_eq!(fork_failure(refusal), Failure::NoConversation);
        assert!(matches!(
            fork_failure("RPC error -32603: boom"),
            Failure::Fork(_)
        ));
    }

    #[test]
    fn every_failure_reads_as_a_sentence_without_an_error_code_in_front() {
        for failure in [
            Failure::NoCopilot,
            Failure::Start("x".into()),
            Failure::NoConversation,
            Failure::Fork("x".into()),
            Failure::Session("x".into()),
            Failure::Timeout,
            Failure::Empty,
        ] {
            let text = failure.to_string();
            assert!(
                text.chars().next().is_some_and(|c| c.is_alphabetic()),
                "{text}"
            );
        }
    }
}
