//! Generation/namespace oracle for retries of a selected expiration hint.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Replace,
    Retag,
    Configure(bool),
    Recreate,
    Select,
    Expire,
    Clock(bool),
    Reopen,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    Success,
    Selected(bool),
    NoSuchKey,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Selection {
    bucket: u8,
    object: u8,
    rules: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Model {
    pub(super) bucket: u8,
    pub(super) object: u8,
    pub(super) rules: u8,
    pub(super) live: bool,
    pub(super) enabled: bool,
    pub(super) due: bool,
    selected: Selection,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            bucket: 0,
            object: 0,
            rules: 0,
            live: true,
            enabled: true,
            due: true,
            selected: Selection {
                bucket: 0,
                object: 0,
                rules: 0,
            },
        }
    }
}

impl Model {
    pub(super) fn apply(&mut self, action: Action) -> Outcome {
        match action {
            Action::Replace => {
                self.object = self.object.checked_add(1).unwrap();
                self.live = true;
            }
            Action::Retag => {
                if !self.live {
                    return Outcome::NoSuchKey;
                }

                self.object = self.object.checked_add(1).unwrap();
            }
            Action::Configure(enabled) => {
                self.rules = self.rules.checked_add(1).unwrap();
                self.enabled = enabled;
            }
            Action::Recreate => {
                self.bucket = self.bucket.checked_add(1).unwrap();
                self.object = self.object.checked_add(1).unwrap();
                self.rules = self.rules.checked_add(1).unwrap();
                self.live = true;
                self.enabled = true;
            }
            Action::Select => {
                let eligible = self.live && self.enabled && self.due;

                if eligible {
                    self.selected = self.current();
                }

                // An empty new page cannot validate an older hint retained by a
                // competing worker; that worker still has to revalidate it.
                return Outcome::Selected(eligible);
            }
            Action::Expire => {
                if self.live && self.enabled && self.due && self.selected == self.current() {
                    self.live = false;
                }
            }
            Action::Clock(due) => self.due = due,
            Action::Reopen => {}
        }

        Outcome::Success
    }

    fn current(self) -> Selection {
        Selection {
            bucket: self.bucket,
            object: self.object,
            rules: self.rules,
        }
    }
}

#[test]
fn identical_values_with_new_identities_do_not_validate_an_old_expiration_hint() {
    for changed in [
        Action::Replace,
        Action::Retag,
        Action::Configure(true),
        Action::Recreate,
    ] {
        let mut model = Model::default();
        model.apply(changed);
        model.apply(Action::Expire);
        assert!(model.live, "{changed:?}");
        assert_eq!(model.apply(Action::Select), Outcome::Selected(true));
        model.apply(Action::Expire);
        assert!(!model.live);
    }
}

#[test]
fn clock_reversal_and_reopen_do_not_authorize_or_forget_a_selected_retry() {
    let mut model = Model::default();
    model.apply(Action::Clock(false));
    model.apply(Action::Reopen);
    model.apply(Action::Expire);
    assert!(model.live);
    assert_eq!(model.apply(Action::Select), Outcome::Selected(false));
    model.apply(Action::Clock(true));
    model.apply(Action::Expire);
    assert!(!model.live);
    assert_eq!(model.apply(Action::Retag), Outcome::NoSuchKey);
}

#[test]
fn reenabling_rules_requires_a_fresh_selection_even_when_values_are_restored() {
    let mut model = Model::default();
    model.apply(Action::Configure(false));
    model.apply(Action::Expire);
    assert!(model.live);
    model.apply(Action::Configure(true));
    model.apply(Action::Expire);
    assert!(model.live);
    model.apply(Action::Select);
    model.apply(Action::Expire);
    assert!(!model.live);
}
