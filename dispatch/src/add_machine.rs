//! The `^a m` overlay: a target, a name, and proof that the machine answers.
//!
//! Its own module because it is a small state machine with a background
//! check in the middle, and none of that needs the rest of the application
//! to be tested.

use std::sync::mpsc::{Receiver, TryRecvError};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use dispatch_client::{Client, Liveness};
use dispatch_config::machines::{self, Machine};
use dispatch_proto::Role;
use dispatch_tui::{Note, Prompt};

/// What a check hands back: a connected client, or why there is none.
pub type Answer = Result<Client, String>;

/// How the overlay proves a machine answers: dials it and hands back the
/// receiver that will carry the [`Answer`].
///
/// A named alias rather than the bare type at every use, which clippy scores
/// too complex to read in place.
pub type Checker = Box<dyn Fn(&Machine) -> Receiver<Answer>>;

/// What the application should do after a key.
#[derive(Debug)]
pub enum Step {
    /// Nothing.
    Stay,
    /// Close the overlay.
    Close,
    /// Dial this machine, then hand the answer to [`AddMachine::checking`].
    Check(Machine),
}

/// Where a check stands.
pub enum Checked {
    /// No answer yet, or no check running.
    Waiting,
    /// It failed; the overlay is asking again and says why.
    Failed,
    /// The machine answered: save it and attach this client.
    Passed(Machine, Client),
}

/// The overlay's state.
pub struct AddMachine {
    prompt: Prompt,
    stage: Stage,
}

enum Stage {
    /// Asking where ssh should connect, remembering a name the user already
    /// chose if a failed check brought them back here.
    Target { name: Option<String> },
    /// Asking what to call it.
    Name { target: String },
    /// Waiting on the check.
    Checking {
        machine: Machine,
        answer: Receiver<Answer>,
    },
}

/// The first question.
fn target_prompt(target: &str) -> Prompt {
    Prompt::new(
        "Add a machine",
        "an ssh target: host, user@host, or an ssh alias",
    )
    .with_input(target)
}

/// The second question.
fn name_prompt(name: &str) -> Prompt {
    Prompt::new("Call it", "letters, digits, - and _").with_input(name)
}

impl Default for AddMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl AddMachine {
    /// A fresh overlay, asking for a target.
    #[must_use]
    pub fn new() -> Self {
        Self {
            prompt: target_prompt(""),
            stage: Stage::Target { name: None },
        }
    }

    /// What to draw.
    #[must_use]
    pub fn prompt(&self) -> &Prompt {
        &self.prompt
    }

    /// Acts on one key.
    ///
    /// `validate` answers whether a name could be registered, so a taken name
    /// is refused before thirty seconds are spent dialling.
    pub fn key(&mut self, key: &KeyEvent, validate: impl Fn(&str) -> Result<(), String>) -> Step {
        if key.code == KeyCode::Esc {
            return Step::Close;
        }

        // Keys wait for the answer: an edit now would describe a machine
        // other than the one being dialled.
        if matches!(self.stage, Stage::Checking { .. }) {
            return Step::Stay;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Backspace => self.prompt.backspace(),
            KeyCode::Char(c) if !ctrl => self.prompt.push(c),
            KeyCode::Enter => return self.submit(validate),
            _ => {}
        }
        Step::Stay
    }

    /// Accepts what is typed, when something is.
    fn submit(&mut self, validate: impl Fn(&str) -> Result<(), String>) -> Step {
        let Some(answer) = self.prompt.answer().map(str::to_string) else {
            return Step::Stay;
        };

        match &self.stage {
            Stage::Target { name } => {
                let name = name
                    .clone()
                    .or_else(|| machines::default_name(&answer))
                    .unwrap_or_default();
                self.prompt = name_prompt(&name);
                self.stage = Stage::Name { target: answer };
                Step::Stay
            }
            Stage::Name { target } => {
                if let Err(reason) = validate(&answer) {
                    self.prompt.set_note(Some(Note::Error(reason)));
                    return Step::Stay;
                }
                let machine = Machine::new(answer, target.clone());
                self.prompt
                    .set_note(Some(Note::Busy(format!("checking {}…", machine.name))));
                Step::Check(machine)
            }
            Stage::Checking { .. } => Step::Stay,
        }
    }

    /// Waits on a check the application has started.
    pub fn checking(&mut self, machine: Machine, answer: Receiver<Answer>) {
        self.stage = Stage::Checking { machine, answer };
    }

    /// Where the check stands.
    ///
    /// A failure goes back to the target, not the name: a dial that fails is
    /// almost always the target's fault, and the name chosen is kept for the
    /// next try.
    pub fn poll(&mut self) -> Checked {
        let Stage::Checking { answer, .. } = &self.stage else {
            return Checked::Waiting;
        };

        let result = match answer.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return Checked::Waiting,
            Err(TryRecvError::Disconnected) => Err("the check ended without an answer".into()),
        };

        let Stage::Checking { machine, .. } =
            std::mem::replace(&mut self.stage, Stage::Target { name: None })
        else {
            return Checked::Waiting;
        };

        match result {
            Ok(client) => Checked::Passed(machine, client),
            Err(reason) => {
                self.prompt = target_prompt(&machine.target);
                self.prompt.set_note(Some(Note::Error(reason)));
                self.stage = Stage::Target {
                    name: Some(machine.name),
                };
                Checked::Failed
            }
        }
    }
}

/// Dials `machine` on a thread of its own and hands the answer back.
///
/// A thread because the dial may take thirty seconds and the interface must
/// keep drawing. If the overlay has been closed by the time it answers, the
/// send fails and the client is dropped with it, which ends its command.
pub fn check(machine: &Machine) -> Receiver<Answer> {
    let (done, answer) = std::sync::mpsc::channel();
    let (program, args) = machine.dial();

    std::thread::spawn(move || {
        let result = Client::attach_over(
            Role::Interface,
            crate::CLIENT_NAME,
            Liveness::default(),
            program,
            args,
        )
        .map_err(|error| error.to_string());
        let _ = done.send(result);
    });

    answer
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_text(add: &mut AddMachine, text: &str) {
        for c in text.chars() {
            let _ = add.key(&key(KeyCode::Char(c)), |_| Ok(()));
        }
    }

    #[test]
    fn a_target_is_followed_by_a_name_that_defaults_from_it() {
        let mut add = AddMachine::new();
        type_text(&mut add, "me@tower.lan");

        assert!(matches!(
            add.key(&key(KeyCode::Enter), |_| Ok(())),
            Step::Stay
        ));
        assert_eq!(add.prompt().input(), "tower");

        let Step::Check(machine) = add.key(&key(KeyCode::Enter), |_| Ok(())) else {
            panic!("a valid name starts the check");
        };
        assert_eq!(machine, Machine::new("tower", "me@tower.lan"));
    }

    #[test]
    fn enter_on_an_empty_target_does_nothing() {
        let mut add = AddMachine::new();
        type_text(&mut add, "  ");

        assert!(matches!(
            add.key(&key(KeyCode::Enter), |_| Ok(())),
            Step::Stay
        ));
        assert_eq!(add.prompt().input(), "  ", "still on the target");
    }

    #[test]
    fn a_name_that_is_refused_says_why_and_dials_nothing() {
        let mut add = AddMachine::new();
        type_text(&mut add, "tower");
        let _ = add.key(&key(KeyCode::Enter), |_| Ok(()));

        let step = add.key(&key(KeyCode::Enter), |_| {
            Err("tower is already registered".into())
        });

        assert!(matches!(step, Step::Stay));
        assert_eq!(
            add.prompt().note(),
            Some(&Note::Error("tower is already registered".into()))
        );
    }

    #[test]
    fn a_failed_check_goes_back_to_the_target_with_the_reason() {
        let mut add = AddMachine::new();
        type_text(&mut add, "towr");
        let _ = add.key(&key(KeyCode::Enter), |_| Ok(()));
        let Step::Check(machine) = add.key(&key(KeyCode::Enter), |_| Ok(())) else {
            panic!("the check starts");
        };

        let (done, answer) = std::sync::mpsc::channel();
        add.checking(machine, answer);
        done.send(Err("ssh: Could not resolve hostname towr".into()))
            .expect("the overlay is listening");

        assert!(matches!(add.poll(), Checked::Failed));
        assert_eq!(add.prompt().input(), "towr", "the target, ready to fix");
        assert_eq!(
            add.prompt().note(),
            Some(&Note::Error("ssh: Could not resolve hostname towr".into()))
        );
    }

    #[test]
    fn keys_wait_while_checking_except_escape() {
        let mut add = AddMachine::new();
        type_text(&mut add, "tower");
        let _ = add.key(&key(KeyCode::Enter), |_| Ok(()));
        let Step::Check(machine) = add.key(&key(KeyCode::Enter), |_| Ok(())) else {
            panic!("the check starts");
        };
        let (_done, answer) = std::sync::mpsc::channel();
        add.checking(machine, answer);

        type_text(&mut add, "zzz");
        assert_eq!(add.prompt().input(), "tower", "typing waits for the answer");
        assert!(matches!(add.poll(), Checked::Waiting));
        assert!(matches!(
            add.key(&key(KeyCode::Esc), |_| Ok(())),
            Step::Close
        ));
    }
}
