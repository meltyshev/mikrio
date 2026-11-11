//! A single-key register oracle, independent of SQL, guards and blob allocation.

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(super) enum Condition {
    Any,
    Absent,
    Matches(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(super) enum Action {
    CreateBucket,
    DeleteBucket,
    HeadBucket,
    Put(u8, Condition),
    Get,
    Head,
    Tags(Option<u8>),
    Delete(Option<u8>),
    Close,
}

impl Action {
    pub(super) fn mutates(self) -> bool {
        matches!(
            self,
            Self::CreateBucket
                | Self::DeleteBucket
                | Self::Put(..)
                | Self::Tags(..)
                | Self::Delete(..)
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(super) struct Object {
    pub value: u8,
    pub tag: Option<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(super) enum Response {
    Success,
    Object(Object),
    MissingBucket,
    MissingKey,
    NonemptyBucket,
    Precondition,
    Closed,
    /// A mutation returned no authoritative commit decision. It may have
    /// performed its complete logical effect or left the register unchanged.
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Default)]
pub(super) struct Model {
    pub bucket: bool,
    pub object: Option<Object>,
    pub closed: [bool; 3],
}

impl Model {
    #[allow(
        clippy::unreachable,
        reason = "test oracle handles these actions before object lookup"
    )]
    fn apply(&mut self, connection: usize, action: Action) -> Response {
        if action == Action::Close {
            self.closed[connection] = true;
            return Response::Success;
        }

        if self.closed[connection] {
            return Response::Closed;
        }

        // The fixture uses the legacy us-east-1 repeated-create rule.
        if action == Action::CreateBucket {
            self.bucket = true;
            return Response::Success;
        }

        if !self.bucket {
            return Response::MissingBucket;
        }

        match action {
            Action::DeleteBucket => {
                if self.object.is_some() {
                    return Response::NonemptyBucket;
                }

                self.bucket = false;
                Response::Success
            }
            Action::HeadBucket => Response::Success,
            Action::Put(value, condition) => {
                match (condition, self.object) {
                    (Condition::Absent, Some(_)) => return Response::Precondition,
                    (Condition::Matches(_), None) => return Response::MissingKey,
                    (Condition::Matches(expected), Some(current)) if current.value != expected => {
                        return Response::Precondition;
                    }
                    _ => {}
                }

                self.object = Some(Object { value, tag: None });
                Response::Success
            }
            Action::Get | Action::Head => {
                self.object.map_or(Response::MissingKey, Response::Object)
            }
            Action::Tags(tag) => {
                let Some(object) = &mut self.object else {
                    return Response::MissingKey;
                };

                object.tag = tag;
                Response::Success
            }
            Action::Delete(expected) => {
                if let Some(expected) = expected {
                    let Some(object) = self.object else {
                        return Response::MissingKey;
                    };

                    if object.value != expected {
                        return Response::Precondition;
                    }
                }

                self.object = None;
                Response::Success
            }
            Action::CreateBucket | Action::Close => unreachable!("handled before lookup"),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
pub(super) struct Entry {
    pub connection: usize,
    pub action: Action,
    pub invoked: usize,
    pub responded: usize,
    pub response: Response,
}

/// Enumerate legal serializations, respecting completed-before-invoked edges.
/// The caller bounds each overlapping window to four operations plus a final read.
pub(super) fn reconcile(initial: Model, entries: &[Entry]) -> Option<Model> {
    assert!(entries.len() <= 6);
    assert!(entries.iter().all(|entry| entry.invoked < entry.responded));
    search(initial, entries, 0)
}

fn search(model: Model, entries: &[Entry], consumed: usize) -> Option<Model> {
    if consumed == (1 << entries.len()) - 1 {
        return Some(model);
    }

    for (index, entry) in entries.iter().enumerate() {
        if consumed & (1 << index) != 0 {
            continue;
        }

        let predecessor_pending = entries.iter().enumerate().any(|(other, previous)| {
            consumed & (1 << other) == 0 && previous.responded < entry.invoked
        });

        if predecessor_pending {
            continue;
        }

        if entry.response == Response::Unknown {
            if !entry.action.mutates() {
                continue;
            }

            if let Some(final_model) = search(model, entries, consumed | (1 << index)) {
                return Some(final_model);
            }
        }

        let mut next = model;
        let response = next.apply(entry.connection, entry.action);

        if response != entry.response
            && !(entry.response == Response::Unknown && response == Response::Success)
        {
            continue;
        }

        if let Some(final_model) = search(next, entries, consumed | (1 << index)) {
            return Some(final_model);
        }
    }

    None
}

#[test]
fn checker_rejects_lost_updates_stale_reads_and_impossible_create_only_winners() {
    let initial = Model {
        bucket: true,
        ..Model::default()
    };

    let put = Entry {
        connection: 0,
        action: Action::Put(1, Condition::Any),
        invoked: 0,
        responded: 1,
        response: Response::Success,
    };

    let stale = Entry {
        connection: 1,
        action: Action::Get,
        invoked: 2,
        responded: 3,
        response: Response::MissingKey,
    };

    assert!(reconcile(initial, &[put, stale]).is_none());

    let overlapping = Entry {
        invoked: 0,
        responded: 3,
        ..stale
    };

    assert!(reconcile(initial, &[put, overlapping]).is_some());

    let exclusive = Entry {
        action: Action::Put(1, Condition::Absent),
        responded: 3,
        ..put
    };

    let impossible = Entry {
        connection: 1,
        action: Action::Put(2, Condition::Absent),
        invoked: 1,
        responded: 2,
        response: Response::Success,
    };

    assert!(reconcile(initial, &[exclusive, impossible]).is_none());

    let replacement = Entry {
        action: Action::Put(2, Condition::Any),
        invoked: 2,
        responded: 3,
        ..impossible
    };

    let lost = Entry {
        action: Action::Head,
        invoked: 4,
        responded: 5,
        response: Response::Object(Object {
            value: 1,
            tag: None,
        }),
        ..stale
    };

    assert!(reconcile(initial, &[put, replacement, lost]).is_none());
}

#[test]
fn unknown_mutations_allow_only_complete_histories_consistent_with_observations() {
    let initial = Model {
        bucket: true,
        object: Some(Object {
            value: 1,
            tag: None,
        }),
        ..Model::default()
    };

    let unknown = Entry {
        connection: 0,
        action: Action::Put(2, Condition::Any),
        invoked: 0,
        responded: 1,
        response: Response::Unknown,
    };

    let observed = Entry {
        connection: 1,
        action: Action::Get,
        invoked: 2,
        responded: 3,
        response: Response::Object(Object {
            value: 1,
            tag: None,
        }),
    };

    assert!(reconcile(initial, &[unknown, observed]).is_some());

    let committed = Entry {
        response: Response::Object(Object {
            value: 2,
            tag: None,
        }),
        ..observed
    };

    assert!(reconcile(initial, &[unknown, committed]).is_some());

    let impossible = Entry {
        response: Response::Object(Object {
            value: 3,
            tag: None,
        }),
        ..observed
    };

    assert!(reconcile(initial, &[unknown, impossible]).is_none());

    let nonmutation = Entry {
        action: Action::Get,
        ..unknown
    };

    assert!(reconcile(initial, &[nonmutation]).is_none());
}

#[test]
fn later_replacement_cannot_erase_an_observed_unknown_publication() {
    let initial = Model {
        bucket: true,
        ..Model::default()
    };

    let unknown = Entry {
        connection: 0,
        action: Action::Put(1, Condition::Any),
        invoked: 0,
        responded: 6,
        response: Response::Unknown,
    };

    let observed = Entry {
        connection: 1,
        action: Action::Get,
        invoked: 1,
        responded: 2,
        response: Response::Object(Object {
            value: 1,
            tag: None,
        }),
    };

    let replacement = Entry {
        connection: 1,
        action: Action::Put(2, Condition::Any),
        invoked: 3,
        responded: 4,
        response: Response::Success,
    };

    let current = Entry {
        invoked: 7,
        responded: 8,
        response: Response::Object(Object {
            value: 2,
            tag: None,
        }),
        ..observed
    };

    assert!(reconcile(initial, &[unknown, observed, replacement, current]).is_some());

    let stale = Entry {
        response: observed.response,
        ..current
    };

    assert!(reconcile(initial, &[unknown, observed, replacement, stale]).is_none());
}
