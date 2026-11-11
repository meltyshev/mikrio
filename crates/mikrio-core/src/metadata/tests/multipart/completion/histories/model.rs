//! A bounded upload oracle independent of SQL, fingerprints and storage helpers.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Select,
    Replace,
    Claim(u8),
    Complete(u8),
    Recover(u8),
    Abort,
    Reopen,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    Success,
    Collected(bool),
    InvalidPart,
    OperationAborted,
    NoSuchUpload,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Upload {
    Open,
    Completing(u8),
    Aborted,
    Completed(u8),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Model {
    pub(super) upload: Upload,
    pub(super) part_generation: u8,
    pub(super) selected_generation: u8,
    pub(super) reserved: Vec<u8>,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            upload: Upload::Open,
            part_generation: 0,
            selected_generation: 0,
            reserved: Vec::new(),
        }
    }
}

impl Model {
    #[allow(
        clippy::unreachable,
        reason = "terminal and recovery actions leave before the open-upload transition"
    )]
    pub(super) fn apply(&mut self, action: Action) -> Outcome {
        if action == Action::Reopen {
            return Outcome::Success;
        }

        if let Action::Recover(owner) = action {
            let Some(index) = self.reserved.iter().position(|current| *current == owner) else {
                return Outcome::Collected(false);
            };

            if self.upload == Upload::Completed(owner) {
                return Outcome::Collected(false);
            }

            self.reserved.remove(index);

            if self.upload == Upload::Completing(owner) {
                self.upload = Upload::Open;
            }

            return Outcome::Collected(true);
        }

        if matches!(self.upload, Upload::Aborted | Upload::Completed(_)) {
            return Outcome::NoSuchUpload;
        }

        if action == Action::Abort {
            self.upload = Upload::Aborted;
            return Outcome::Success;
        }

        if let Action::Complete(owner) = action {
            if self.upload != Upload::Completing(owner) {
                return Outcome::OperationAborted;
            }

            self.upload = Upload::Completed(owner);
            return Outcome::Success;
        }

        if self.upload != Upload::Open {
            return Outcome::OperationAborted;
        }

        match action {
            Action::Select => self.selected_generation = self.part_generation,
            Action::Replace => self.part_generation = self.part_generation.checked_add(1).unwrap(),
            Action::Claim(owner) => {
                if self.selected_generation != self.part_generation {
                    return Outcome::InvalidPart;
                }

                // Each fixture claim has a fresh output identity. Reuse after a
                // failed admission is not an application idempotency contract.
                assert!(!self.reserved.contains(&owner));
                self.reserved.push(owner);
                self.upload = Upload::Completing(owner);
            }
            _ => unreachable!("terminal and recovery actions are handled first"),
        }

        Outcome::Success
    }
}

#[test]
fn same_bytes_do_not_authorize_a_stale_part_generation() {
    let mut model = Model::default();
    assert_eq!(model.apply(Action::Replace), Outcome::Success);
    assert_eq!(model.apply(Action::Claim(1)), Outcome::InvalidPart);
    assert_eq!(model.upload, Upload::Open);
    assert_eq!(model.reserved, Vec::<u8>::new());
    assert_eq!(model.apply(Action::Select), Outcome::Success);
    assert_eq!(model.apply(Action::Claim(2)), Outcome::Success);
}

#[test]
fn abort_and_recovery_prevent_old_claims_from_publishing() {
    for action in [Action::Abort, Action::Recover(1)] {
        let mut model = Model::default();
        assert_eq!(model.apply(Action::Claim(1)), Outcome::Success);
        model.apply(action);
        assert_ne!(model.apply(Action::Complete(1)), Outcome::Success);
        assert!(!matches!(model.upload, Upload::Completed(_)));
    }
}

#[test]
fn a_peer_claim_cannot_publish_or_be_reset_by_another_owner() {
    let mut model = Model::default();
    model.apply(Action::Claim(1));
    assert_eq!(model.apply(Action::Complete(2)), Outcome::OperationAborted);
    assert_eq!(model.apply(Action::Recover(2)), Outcome::Collected(false));
    assert_eq!(model.upload, Upload::Completing(1));
    assert_eq!(model.apply(Action::Complete(1)), Outcome::Success);
    assert_eq!(model.apply(Action::Recover(1)), Outcome::Collected(false));
    assert_eq!(model.upload, Upload::Completed(1));
}
