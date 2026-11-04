//! The address book: people and their groups, kept on this computer (contacts.json) and, with
//! the Google script, in step with Google Contacts both ways. Changes made here are marked
//! "dirty" and sent to Google at the next sync; then everything is read back from Google.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Person {
    /// Google's id ("people/c123…") or, before it reached Google, "local:…"
    pub id: String,
    pub name: String,
    pub emails: Vec<String>,
    /// ids of the groups the person is in
    pub groups: Vec<String>,
    /// changed here and not yet sent to Google
    pub dirty: bool,
    pub deleted: bool,
    /// a number that grows with every change made here (to tell newer edits from older ones)
    pub rev: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub dirty: bool,
    pub deleted: bool,
    pub rev: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Book {
    pub people: Vec<Person>,
    pub groups: Vec<Group>,
    /// when it was last in step with Google (text for the person)
    pub synced: String,
    pub next_rev: u64,
}

fn path() -> std::path::PathBuf {
    if let Ok(d) = std::env::var("POCZTA_DATA_DIR") {
        return std::path::PathBuf::from(d).join("contacts.json");
    }
    crate::model::cache_dir().parent().map(|p| p.join("contacts.json")).unwrap_or_else(|| "contacts.json".into())
}

pub fn new_id() -> String {
    format!("local:{}", crate::share::new_token())
}

impl Book {
    pub fn load() -> Book {
        std::fs::read_to_string(path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(t) = serde_json::to_string(self) {
            let _ = std::fs::write(path(), t);
        }
    }

    fn rev(&mut self) -> u64 {
        self.next_rev += 1;
        self.next_rev
    }

    pub fn people(&self) -> impl Iterator<Item = &Person> {
        self.people.iter().filter(|p| !p.deleted)
    }

    pub fn groups(&self) -> Vec<&Group> {
        let mut g: Vec<&Group> = self.groups.iter().filter(|g| !g.deleted).collect();
        g.sort_by_key(|g| g.name.to_lowercase());
        g
    }

    pub fn members(&self, group: &str) -> Vec<&Person> {
        let mut v: Vec<&Person> = self.people().filter(|p| p.groups.iter().any(|g| g == group)).collect();
        v.sort_by_key(|p| p.name.to_lowercase());
        v
    }

    /// Every address of a group's people (for writing to the whole group).
    pub fn group_addresses(&self, group: &str) -> Vec<String> {
        self.members(group)
            .iter()
            .filter_map(|p| {
                let e = p.emails.first()?;
                Some(if p.name.trim().is_empty() { e.clone() } else { format!("{} <{}>", p.name.trim(), e) })
            })
            .collect()
    }

    pub fn person_by_email(&self, email: &str) -> Option<&Person> {
        let e = email.trim().to_lowercase();
        self.people().find(|p| p.emails.iter().any(|x| x.trim().to_lowercase() == e))
    }

    /// A person changed here (added or edited).
    pub fn put_person(&mut self, mut p: Person) {
        p.dirty = true;
        p.rev = self.rev();
        match self.people.iter_mut().find(|x| x.id == p.id) {
            Some(x) => *x = p,
            None => self.people.push(p),
        }
    }

    pub fn delete_person(&mut self, id: &str) {
        let rev = self.rev();
        if let Some(p) = self.people.iter_mut().find(|x| x.id == id) {
            p.deleted = true;
            p.dirty = true;
            p.rev = rev;
        }
    }

    pub fn put_group(&mut self, id: Option<&str>, name: &str) -> String {
        let rev = self.rev();
        let id = id.map(String::from).unwrap_or_else(new_id);
        match self.groups.iter_mut().find(|g| g.id == id) {
            Some(g) => {
                g.name = name.to_string();
                g.dirty = true;
                g.rev = rev;
            }
            None => self.groups.push(Group { id: id.clone(), name: name.to_string(), dirty: true, deleted: false, rev }),
        }
        id
    }

    pub fn delete_group(&mut self, id: &str) {
        let rev = self.rev();
        if let Some(g) = self.groups.iter_mut().find(|g| g.id == id) {
            g.deleted = true;
            g.dirty = true;
            g.rev = rev;
        }
        // the people stay; only their membership goes
        let ids: Vec<String> = self.people.iter().filter(|p| p.groups.iter().any(|g| g == id)).map(|p| p.id.clone()).collect();
        for pid in ids {
            if let Some(mut p) = self.people.iter().find(|p| p.id == pid).cloned() {
                p.groups.retain(|g| g != id);
                self.put_person(p);
            }
        }
    }

    /// Adds `email` (with `name`) to a group: the person already in the book, or a new one.
    pub fn add_to_group(&mut self, group: &str, name: &str, email: &str) {
        let mut p = self.person_by_email(email).cloned().unwrap_or_else(|| Person { id: new_id(), name: name.to_string(), emails: vec![email.trim().to_string()], ..Default::default() });
        if p.name.trim().is_empty() {
            p.name = name.to_string();
        }
        if !p.groups.iter().any(|g| g == group) {
            p.groups.push(group.to_string());
        }
        self.put_person(p);
    }

    pub fn remove_from_group(&mut self, group: &str, person: &str) {
        if let Some(mut p) = self.people.iter().find(|p| p.id == person).cloned() {
            p.groups.retain(|g| g != group);
            self.put_person(p);
        }
    }

    pub fn dirty_count(&self) -> usize {
        self.people.iter().filter(|p| p.dirty).count() + self.groups.iter().filter(|g| g.dirty).count()
    }

    /// After a sync: Google's book, with the changes made here meanwhile (newer than what was
    /// sent) laid over it.
    pub fn merge_after_sync(&mut self, mut fresh: Book, sent_rev: u64, renamed: &[(String, String)]) {
        let rename = |id: &str| renamed.iter().find(|(a, _)| a == id).map(|(_, b)| b.clone()).unwrap_or_else(|| id.to_string());
        for g in self.groups.iter().filter(|g| g.dirty && g.rev > sent_rev) {
            let mut g = g.clone();
            g.id = rename(&g.id);
            fresh.groups.retain(|x| x.id != g.id);
            fresh.groups.push(g);
        }
        for p in self.people.iter().filter(|p| p.dirty && p.rev > sent_rev) {
            let mut p = p.clone();
            p.id = rename(&p.id);
            p.groups = p.groups.iter().map(|g| rename(g)).collect();
            fresh.people.retain(|x| x.id != p.id);
            fresh.people.push(p);
        }
        fresh.next_rev = self.next_rev;
        *self = fresh;
    }
}

/// The result of a sync: Google's book, what local ids became, the newest change sent.
pub struct Synced {
    pub book: Book,
    pub renamed: Vec<(String, String)>,
    pub sent_rev: u64,
    pub sent: usize,
}

/// Sends the changes made here to Google Contacts (groups first, then people), then reads
/// everything back. `book` is a copy of the book when the sync started.
pub fn sync(acc: &crate::model::Account, book: &Book) -> Result<Synced, String> {
    let token = crate::google::token(acc)?;
    let sent_rev = book.next_rev;
    let mut renamed: Vec<(String, String)> = Vec::new();
    let mut sent = 0;
    for g in book.groups.iter().filter(|g| g.dirty) {
        let local = g.id.starts_with("local:");
        if g.deleted {
            if !local {
                crate::google::delete_group(&token, &g.id)?;
            }
        } else {
            let id = crate::google::save_group(&token, if local { "" } else { &g.id }, &g.name)?;
            if local {
                renamed.push((g.id.clone(), id));
            }
        }
        sent += 1;
    }
    let rename = |id: &str, renamed: &[(String, String)]| renamed.iter().find(|(a, _)| a == id).map(|(_, b)| b.clone()).unwrap_or_else(|| id.to_string());
    for p in book.people.iter().filter(|p| p.dirty) {
        let local = p.id.starts_with("local:");
        if p.deleted {
            if !local {
                crate::google::delete_person(&token, &p.id)?;
            }
        } else {
            let groups: Vec<String> = p.groups.iter().map(|g| rename(g, &renamed)).filter(|g| !g.starts_with("local:")).collect();
            let id = crate::google::save_person(&token, if local { "" } else { &p.id }, &p.name, &p.emails, &groups)?;
            if local {
                renamed.push((p.id.clone(), id));
            }
        }
        sent += 1;
    }
    let (groups, people) = crate::google::contacts(&token)?;
    let mut fresh = Book::default();
    for (id, name) in groups {
        fresh.groups.push(Group { id, name, ..Default::default() });
    }
    for (id, name, emails, groups) in people {
        fresh.people.push(Person { id, name, emails, groups, ..Default::default() });
    }
    fresh.synced = chrono::Local::now().format("%d.%m %H:%M").to_string();
    Ok(Synced { book: fresh, renamed, sent_rev, sent })
}
