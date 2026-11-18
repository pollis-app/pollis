//! What changed between two manifests (#2005): the update diff an admin sees,
//! and the rule a development installation's builds are held to.
//!
//! A **widening** is anything that lets the App see or do more than before: a
//! new trigger kind, a new channel selector, a longer history window, a new
//! event kind, a new action, a new egress host or a larger state quota. The
//! approval page always highlights widenings and never pre-accepts them; an
//! update with none can be approved with one tap. A development build whose
//! manifest widens the approved one is refused by the runtime.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{AdminGrants, Manifest};

/// Which part of the manifest a change is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeField {
    Trigger,
    Channel,
    History,
    Event,
    Action,
    Egress,
    StateQuota,
}

/// Which way a change goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Removed,
    Increased,
    Decreased,
}

/// One change between two manifests. `value` is the item added or removed
/// (`"#ops"`, `"api.linear.app"`, `"post"`) or, for a number, the new value;
/// `previous` is the old number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestChange {
    pub field: ChangeField,
    pub kind: ChangeKind,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
    /// The App may see or do more than before.
    pub widening: bool,
}

fn set_changes<T: Ord + Clone>(
    field: ChangeField,
    old: &BTreeSet<T>,
    new: &BTreeSet<T>,
    name: impl Fn(&T) -> String,
    out: &mut Vec<ManifestChange>,
) {
    for added in new.difference(old) {
        out.push(ManifestChange { field, kind: ChangeKind::Added, value: name(added), previous: None, widening: true });
    }
    for removed in old.difference(new) {
        out.push(ManifestChange { field, kind: ChangeKind::Removed, value: name(removed), previous: None, widening: false });
    }
}

fn number_change(field: ChangeField, old: u64, new: u64, out: &mut Vec<ManifestChange>) {
    if new == old {
        return;
    }
    let kind = if new > old { ChangeKind::Increased } else { ChangeKind::Decreased };
    out.push(ManifestChange {
        field,
        kind,
        value: new.to_string(),
        previous: Some(old.to_string()),
        widening: new > old,
    });
}

/// Every change from `old` to `new`, widenings flagged. Name, version and
/// description changes are not listed: they grant nothing.
pub fn diff(old: &Manifest, new: &Manifest) -> Vec<ManifestChange> {
    let mut out = Vec::new();
    set_changes(ChangeField::Trigger, &old.triggers, &new.triggers, |t| t.as_str().to_string(), &mut out);

    // A named channel is new unless the old manifest already took any channel ("*").
    let old_channels: BTreeSet<String> = old.read.channels.iter().map(|c| normalize_selector(c)).collect();
    let new_channels: BTreeSet<String> = new.read.channels.iter().map(|c| normalize_selector(c)).collect();
    let old_any = old_channels.contains("*");
    for added in new_channels.difference(&old_channels) {
        out.push(ManifestChange {
            field: ChangeField::Channel,
            kind: ChangeKind::Added,
            value: added.clone(),
            previous: None,
            widening: !old_any,
        });
    }
    for removed in old_channels.difference(&new_channels) {
        out.push(ManifestChange {
            field: ChangeField::Channel,
            kind: ChangeKind::Removed,
            value: removed.clone(),
            previous: None,
            widening: false,
        });
    }

    number_change(ChangeField::History, u64::from(old.read.history), u64::from(new.read.history), &mut out);
    set_changes(ChangeField::Event, &old.read.events, &new.read.events, |e| e.as_str().to_string(), &mut out);
    set_changes(ChangeField::Action, &old.actions, &new.actions, |a| a.as_str().to_string(), &mut out);
    set_changes(ChangeField::Egress, &old.egress, &new.egress, Clone::clone, &mut out);
    number_change(ChangeField::StateQuota, old.state.quota, new.state.quota, &mut out);
    out
}

/// Only the widenings from `old` to `new`.
pub fn widenings_between(old: &Manifest, new: &Manifest) -> Vec<ManifestChange> {
    diff(old, new).into_iter().filter(|c| c.widening).collect()
}

/// `"#Ops"` and `"ops"` to `"#ops"`; `"*"` stays.
fn normalize_selector(selector: &str) -> String {
    if selector == "*" {
        return "*".to_string();
    }
    format!("#{}", selector.strip_prefix('#').unwrap_or(selector).to_lowercase())
}

impl AdminGrants {
    /// These grants carried to another manifest: everything the new manifest
    /// no longer requests is dropped, nothing is added. A non-widening update
    /// is approved with these, so it never grants more than the admin chose.
    pub fn carried_to(&self, manifest: &Manifest) -> AdminGrants {
        AdminGrants {
            channels: self
                .channels
                .iter()
                .filter(|c| manifest.requests_channel(&c.name))
                .cloned()
                .collect(),
            history: self.history.min(manifest.read.history),
            events: self.events.intersection(&manifest.read.events).copied().collect(),
            actions: self.actions.intersection(&manifest.actions).copied().collect(),
            egress: self.egress.intersection(&manifest.egress).cloned().collect(),
        }
    }

    /// The canonical order: channels sorted by id and deduplicated. Approvals
    /// are signed over grants in this form.
    pub fn normalized(mut self) -> AdminGrants {
        self.channels.sort_by(|a, b| a.id.cmp(&b.id).then_with(|| a.name.cmp(&b.name)));
        self.channels.dedup_by(|a, b| a.id == b.id);
        for c in &mut self.channels {
            c.name = c.name.strip_prefix('#').unwrap_or(&c.name).to_lowercase();
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActionKind, ChannelGrant, EventKind};

    fn m(json: &str) -> Manifest {
        Manifest::from_json(json.as_bytes()).unwrap()
    }

    fn v1() -> Manifest {
        m(r##"{"name":"triage","version":"1.0.0","triggers":["messages"],
               "read":{"channels":["#support"],"history":30},
               "actions":["reply"],"state":{"quota":100},"egress":["api.linear.app"]}"##)
    }

    #[test]
    fn every_kind_of_widening_is_flagged() {
        let v2 = m(r##"{"name":"triage","version":"2.0.0","triggers":["messages","webhook"],
               "read":{"channels":["#support","#ops"],"history":90,"events":["created","edited"]},
               "actions":["reply","post"],"state":{"quota":200},"egress":["api.linear.app","evil.example"]}"##);
        let w = widenings_between(&v1(), &v2);
        let fields: BTreeSet<ChangeField> = w.iter().map(|c| c.field).collect();
        assert_eq!(
            fields,
            BTreeSet::from([
                ChangeField::Trigger,
                ChangeField::Channel,
                ChangeField::History,
                ChangeField::Event,
                ChangeField::Action,
                ChangeField::Egress,
                ChangeField::StateQuota,
            ])
        );
        assert!(w.iter().any(|c| c.field == ChangeField::Egress && c.value == "evil.example"));
        assert!(w.iter().any(|c| c.field == ChangeField::History && c.previous.as_deref() == Some("30")));
    }

    #[test]
    fn a_narrowing_update_has_no_widening() {
        let v2 = m(r##"{"name":"triage","version":"1.1.0","description":"now shorter",
               "triggers":["messages"],"read":{"channels":["#SUPPORT"],"history":7},
               "actions":[],"egress":[]}"##);
        let changes = diff(&v1(), &v2);
        assert!(!changes.is_empty());
        assert!(changes.iter().all(|c| !c.widening), "{changes:?}");
    }

    #[test]
    fn a_wildcard_is_wider_than_names_but_names_are_not_wider_than_a_wildcard() {
        let mut any = v1();
        any.read.channels = vec!["*".into()];
        assert!(widenings_between(&v1(), &any).iter().any(|c| c.field == ChangeField::Channel));
        let mut named = any.clone();
        named.read.channels = vec!["*".into(), "#ops".into()];
        assert!(widenings_between(&any, &named).is_empty());
    }

    #[test]
    fn carried_grants_drop_what_the_new_manifest_no_longer_asks_for() {
        let grants = AdminGrants {
            channels: vec![ChannelGrant { id: "c1".into(), name: "support".into() }],
            history: 30,
            events: BTreeSet::from([EventKind::Created]),
            actions: BTreeSet::from([ActionKind::Reply]),
            egress: BTreeSet::from(["api.linear.app".to_string()]),
        };
        let v2 = m(r##"{"name":"triage","version":"1.1.0","read":{"channels":["#ops"],"history":7}}"##);
        let carried = grants.carried_to(&v2);
        assert!(carried.channels.is_empty());
        assert_eq!(carried.history, 7);
        assert!(carried.actions.is_empty() && carried.egress.is_empty());
        assert!(carried.check_not_wider(&v2).is_ok());
    }
}
