//! Turning a phone reply into the keys that answer Copilot's question form.
//!
//! Copilot's `ask_user` is not a prompt that takes typed text. It is a form: for a
//! field with choices, a list with one row highlighted and an "Other (type your
//! answer)" row at the bottom; for a free-text field, an input prefilled with its
//! default. Enter accepts the field and moves to the next, and on the last field
//! submits the form. Typed text goes nowhere unless the Other row or a text field has
//! the focus. So a reply has to be played as keys, and this module decides which.
//!
//! Every answer is given through a text input rather than by selecting a row, which is
//! what makes it robust: a chosen option is typed verbatim into the "Other" row, so no
//! list read blind has to land on the right row, and the text Copilot records is the
//! same as if the row had been selected. Established by driving the real widget
//! (Copilot 1.0.104):
//!
//! - Down at the Other row stays there (it does not wrap), so one Down per choice plus
//!   one reaches Other from wherever the highlight started;
//! - while Other is highlighted an input is open under it: text typed goes in, Enter
//!   submits it;
//! - a free-text field is prefilled with its default; Ctrl+U clears it before typing;
//! - Enter with nothing typed accepts the highlighted choice or the prefilled default.
//!
//! The planner is pure so every shape of reply can be tested without a terminal. What
//! actually happened is read back from the event log afterwards, because the keys are
//! played without reading the screen between fields.

use super::{Question, QuestionField};

/// One thing to send the pane, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Down,
    Enter,
    /// Erase the prefilled default of a text field.
    ClearLine,
    /// Plain characters, written as one piece.
    Text(String),
    /// Give the form time to move to its next field before the next key lands.
    Settle,
}

/// What one line of the reply means for one field.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Answer {
    /// Take what the form offers: the highlighted choice or the prefilled default.
    Default,
    /// The field's choice at this index.
    Choice(usize),
    /// Text for the Other row or the text field.
    Text(String),
}

/// A line that means "keep the default" for its field, so a later field can still be
/// answered from a phone, where an empty line is hard to type.
const KEEP_DEFAULT: &str = "-";

/// The keys that answer `question` with `reply`, and a plain description of what each
/// field is being answered with, for the confirmation.
pub fn plan(question: &Question, reply: &str) -> (Vec<Key>, Vec<String>) {
    let lines = split_reply(question, reply);
    let mut keys = Vec::new();
    let mut said = Vec::new();
    let mut first_number = 1;
    for (index, field) in question.fields.iter().enumerate() {
        let answer = interpret(field, first_number, lines.get(index).map(String::as_str));
        first_number += field.choices.len();
        if index > 0 {
            keys.push(Key::Settle);
        }
        said.push(describe(field, &answer));
        keys.extend(keys_for(field, &answer));
    }
    if question.fields.is_empty() {
        // Nothing the form can take but a submit; the text itself has nowhere to go.
        keys.push(Key::Enter);
    }
    (keys, said)
}

/// One answer per field. A single-field question takes the whole reply as its answer,
/// line breaks and all; a form takes one line per field, in order.
fn split_reply(question: &Question, reply: &str) -> Vec<String> {
    let lines: Vec<String> = reply
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    if question.fields.len() <= 1 && !lines.is_empty() {
        return vec![lines.join(" ")];
    }
    lines
}

fn interpret(field: &QuestionField, first_number: usize, line: Option<&str>) -> Answer {
    let Some(line) = line else {
        return Answer::Default;
    };
    if line == KEEP_DEFAULT {
        return Answer::Default;
    }
    if field.choices.is_empty() {
        return Answer::Text(line.to_string());
    }
    // A number from the message's numbering, or the choice's own words.
    if let Ok(number) = line.trim_end_matches('.').parse::<usize>() {
        if number >= first_number && number < first_number + field.choices.len() {
            return Answer::Choice(number - first_number);
        }
    }
    if let Some(index) = field
        .choices
        .iter()
        .position(|choice| choice.trim().eq_ignore_ascii_case(line))
    {
        return Answer::Choice(index);
    }
    Answer::Text(line.to_string())
}

fn keys_for(field: &QuestionField, answer: &Answer) -> Vec<Key> {
    // Every answer that is not "keep the default" goes through the field's text input:
    // the "Other (type your answer)" row of a choice field, or the field itself when it
    // is free text. A chosen option is typed in verbatim rather than selected by
    // arrowing to its row — the text Copilot records is the same either way, and typing
    // avoids landing on the wrong row of a list read blind. Reaching Other needs only
    // Down, which stops at the Other row instead of wrapping, so pressing it one more
    // time than there are choices arrives there from wherever the highlight started.
    let mut keys = Vec::new();
    let text = match answer {
        // Accept whatever the form offers: the highlighted choice, or a text field's
        // prefilled default. No navigation, no typing.
        Answer::Default => {
            keys.push(Key::Enter);
            return keys;
        }
        Answer::Choice(index) => field.choices[*index].clone(),
        Answer::Text(text) => text.clone(),
    };
    if field.choices.is_empty() {
        // A free-text field, already holding its default; clear it before typing.
        keys.push(Key::ClearLine);
    } else {
        for _ in 0..field.choices.len() + 1 {
            keys.push(Key::Down);
        }
    }
    keys.push(Key::Text(text));
    keys.push(Key::Enter);
    keys
}

fn describe(field: &QuestionField, answer: &Answer) -> String {
    let label = if field.title.trim().is_empty() {
        String::new()
    } else {
        format!("{}: ", field.title.trim())
    };
    match answer {
        Answer::Default => format!("{label}(default)"),
        Answer::Choice(index) => format!("{label}{}", field.choices[*index].trim()),
        Answer::Text(text) => format!("{label}{text}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choices(title: &str, choices: &[&str]) -> QuestionField {
        QuestionField {
            title: title.to_string(),
            choices: choices.iter().map(|c| c.to_string()).collect(),
            ..QuestionField::default()
        }
    }

    fn text_field(title: &str, default: &str) -> QuestionField {
        QuestionField {
            title: title.to_string(),
            default: Some(default.to_string()),
            ..QuestionField::default()
        }
    }

    fn question(fields: Vec<QuestionField>) -> Question {
        Question {
            tool_call_id: "call".to_string(),
            text: "?".to_string(),
            fields,
        }
    }

    #[test]
    fn a_chosen_option_is_typed_into_other_rather_than_selected_by_row() {
        // Down caps on the Other row, so one Down per choice plus one reaches it from
        // the default wherever that is; the chosen option's text is then typed in, so
        // no list read blind has to land on the right row.
        let q = question(vec![choices("Size", &["Small", "Medium", "Large"])]);
        let (keys, said) = plan(&q, "3");
        assert_eq!(
            keys,
            vec![
                Key::Down,
                Key::Down,
                Key::Down,
                Key::Down,
                Key::Text("Large".to_string()),
                Key::Enter
            ]
        );
        assert_eq!(said, vec!["Size: Large"]);
        // "3." and the choice's own words mean the same.
        assert_eq!(plan(&q, "3.").0, keys);
        assert_eq!(plan(&q, "large").0, keys);
    }

    #[test]
    fn text_that_is_not_a_choice_goes_through_the_other_row_verbatim() {
        let q = question(vec![choices("Size", &["Small", "Medium", "Large"])]);
        let (keys, said) = plan(&q, "none of those, huge");
        assert_eq!(
            keys,
            vec![
                Key::Down,
                Key::Down,
                Key::Down,
                Key::Down,
                Key::Text("none of those, huge".to_string()),
                Key::Enter
            ]
        );
        assert_eq!(said, vec!["Size: none of those, huge"]);
        // A number that is not one of this field's is typed as the text it is.
        assert!(matches!(plan(&q, "7").0.as_slice(), [.., Key::Text(t), Key::Enter] if t == "7"));
    }

    #[test]
    fn a_form_takes_one_line_per_field_and_a_missing_or_dashed_line_keeps_the_default() {
        let q = question(vec![
            choices("Colour", &["Red", "Green", "Blue"]),
            text_field("Finish time", "Monday 09:00"),
            choices("Shape", &["Round", "Square"]),
        ]);
        // Numbers run straight through the form, so the Shape choices are 4 and 5.
        let (keys, said) = plan(&q, "2\nTuesday 17:30\n5");
        assert_eq!(
            keys,
            vec![
                // Colour: Green, typed into Other (3 choices → 4 Downs).
                Key::Down,
                Key::Down,
                Key::Down,
                Key::Down,
                Key::Text("Green".to_string()),
                Key::Enter,
                Key::Settle,
                // Finish time: a text field, cleared then typed.
                Key::ClearLine,
                Key::Text("Tuesday 17:30".to_string()),
                Key::Enter,
                Key::Settle,
                // Shape: Square, into Other (2 choices → 3 Downs).
                Key::Down,
                Key::Down,
                Key::Down,
                Key::Text("Square".to_string()),
                Key::Enter,
            ]
        );
        assert_eq!(
            said,
            vec![
                "Colour: Green",
                "Finish time: Tuesday 17:30",
                "Shape: Square"
            ]
        );

        // Only the first field answered: the rest take what the form offers with a
        // bare Enter.
        let (keys, said) = plan(&q, "1");
        assert_eq!(
            keys,
            vec![
                Key::Down,
                Key::Down,
                Key::Down,
                Key::Down,
                Key::Text("Red".to_string()),
                Key::Enter,
                Key::Settle,
                Key::Enter,
                Key::Settle,
                Key::Enter
            ]
        );
        assert_eq!(
            said,
            vec!["Colour: Red", "Finish time: (default)", "Shape: (default)"]
        );

        // A dash keeps a field's default so a later one can still be answered.
        let (_, said) = plan(&q, "-\n-\nround");
        assert_eq!(
            said,
            vec![
                "Colour: (default)",
                "Finish time: (default)",
                "Shape: Round"
            ]
        );
    }

    #[test]
    fn a_single_field_takes_a_multi_line_reply_as_one_answer() {
        let q = question(vec![text_field("Note", "keep this")]);
        let (keys, said) = plan(&q, "First thought.\n\nSecond thought.");
        assert_eq!(
            keys,
            vec![
                Key::ClearLine,
                Key::Text("First thought. Second thought.".to_string()),
                Key::Enter
            ]
        );
        assert_eq!(said, vec!["Note: First thought. Second thought."]);
    }

    #[test]
    fn an_empty_reply_or_a_question_without_fields_just_accepts_the_form() {
        let q = question(vec![choices("Size", &["Small", "Medium"])]);
        assert_eq!(plan(&q, "   ").0, vec![Key::Enter]);
        let bare = question(vec![]);
        assert_eq!(plan(&bare, "anything").0, vec![Key::Enter]);
    }
}
