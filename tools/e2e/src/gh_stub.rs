//! Stands in for `gh`, for scripts that set `fake_gh`, so thread watching can be driven
//! without an account, a real thread, or a network. The driver copies its own executable
//! into the run's `bin/` under the name `gh`, and this is what that copy does.
//!
//! It answers just enough for CST's thread watcher to do a full round on a discussion:
//! who is logged in, a token, a discussion's `updatedAt`, and an empty comment list.
//! Anything else fails loudly, so a script that wanders off that path finds out.
//!
//! `github down` in a script makes every call fail the way a real `gh` does with no
//! network; `github up` puts it back.

/// The account the fake `gh` is logged in as.
pub const LOGIN: &str = "cst-e2e";

/// The file whose presence means GitHub is out of reach, in the run directory.
pub const DOWN_MARKER: &str = "github-down";

pub fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let run_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.parent()?.to_path_buf()));
    if run_dir.is_some_and(|dir| dir.join(DOWN_MARKER).exists()) {
        // What `gh` itself prints with the network unplugged.
        eprintln!("error connecting to api.github.com");
        eprintln!("check your internet connection or https://githubstatus.com");
        std::process::exit(1);
    }

    let has = |word: &str| args.iter().any(|arg| arg == word);
    let query = args
        .iter()
        .find_map(|arg| arg.strip_prefix("query="))
        .unwrap_or_default();

    if args.first().map(String::as_str) == Some("auth") && has("token") {
        println!("e2e-token");
    } else if has("graphql") && query.contains("{updatedAt}") {
        println!(
            r#"{{"data":{{"repository":{{"discussion":{{"updatedAt":"2026-01-01T00:00:00Z"}}}}}}}}"#
        );
    } else if has("graphql") && query.contains("comments(") {
        println!(r#"{{"data":{{"repository":{{"discussion":{{"comments":{{"nodes":[]}}}}}}}}}}"#);
    } else if has("api") && has("user") {
        println!("{LOGIN}");
    } else {
        eprintln!(
            "fake gh: not something the e2e stub answers: gh {}",
            args.join(" ")
        );
        std::process::exit(1);
    }
}
