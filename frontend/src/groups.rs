//! Presentation rules for group conversations, shared by the UI and its tests.
//! Names are user content and are never translated.
use crate::localization::{Language, translate};
use thiscord_shared::{
    AccountId,
    friends::{Group, GroupMember, SystemEvent, SystemMessage},
};

/// Translates `template` and fills each `{}` with the next argument, in order.
/// Arguments are inserted verbatim (never re-scanned or translated).
fn fill(language: Language, template: &str, args: &[&dyn std::fmt::Display]) -> String {
    let mut parts = translate(language, template).split("{}");
    let mut out = parts.next().unwrap_or_default().to_owned();
    for (part, arg) in parts.zip(args) {
        out.push_str(&arg.to_string());
        out.push_str(part);
    }
    out
}

/// Display name, falling back to the username.
pub fn name(member: &GroupMember) -> &str {
    if member.person.display_name.trim().is_empty() {
        &member.person.username
    } else {
        &member.person.display_name
    }
}

/// The group's name, otherwise the other members' names.
pub fn title(group: &Group, me: Option<AccountId>, language: Language) -> String {
    let named = group.name.trim();
    if !named.is_empty() {
        return named.to_owned();
    }
    let others: Vec<&str> = group
        .members
        .iter()
        .filter(|m| Some(m.person.id) != me)
        .map(name)
        .collect();
    match others.as_slice() {
        [] => translate(language, "Just you").to_owned(),
        [_] | [_, _] | [_, _, _] => others.join(", "),
        [first, second, rest @ ..] => fill(
            language,
            "{}, {} and {} others",
            &[first, second, &rest.len()],
        ),
    }
}

/// Up to two members to stack as the group's avatar, others first.
pub fn stack(group: &Group, me: Option<AccountId>) -> Vec<&GroupMember> {
    let mut shown: Vec<&GroupMember> = group
        .members
        .iter()
        .filter(|m| Some(m.person.id) != me)
        .take(2)
        .collect();
    if shown.is_empty() {
        shown.extend(group.members.first());
    }
    shown
}

/// "N members", "1 member".
pub fn member_count(count: usize, language: Language) -> String {
    if count == 1 {
        translate(language, "1 member").to_owned()
    } else {
        fill(language, "{} members", &[&count])
    }
}

/// Who is typing in a group, by display name.
pub fn typing(names: &[String], language: Language) -> Option<String> {
    Some(match names {
        [] => return None,
        [one] => fill(language, "{} is typing…", &[&one]),
        [one, two] => fill(language, "{} and {} are typing…", &[&one, &two]),
        _ => translate(language, "Several people are typing…").to_owned(),
    })
}

/// One line describing a membership or metadata notice.
pub fn system_text(message: &SystemMessage, language: Language) -> String {
    let actor = message.actor_name.as_str();
    match &message.event {
        SystemEvent::Created {} => fill(language, "{} created the group.", &[&actor]),
        SystemEvent::Added { people } => {
            let names: Vec<&str> = people.iter().map(|p| p.display_name.as_str()).collect();
            fill(
                language,
                "{} added {} to the group.",
                &[&actor, &names.join(", ")],
            )
        }
        SystemEvent::Left {} => fill(language, "{} left the group.", &[&actor]),
        SystemEvent::Removed { person } => fill(
            language,
            "{} removed {} from the group.",
            &[&actor, &person.display_name],
        ),
        SystemEvent::Renamed { name } if name.trim().is_empty() => {
            fill(language, "{} removed the group name.", &[&actor])
        }
        SystemEvent::Renamed { name } => {
            fill(language, "{} renamed the group to {}.", &[&actor, &name])
        }
        SystemEvent::OwnerChanged { person } => fill(
            language,
            "{} is now the group owner.",
            &[&person.display_name],
        ),
    }
}

/// Case-insensitive ordering key for the conversation list.
pub fn sort_key(title: &str) -> String {
    title.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use thiscord_shared::friends::{Named, Person};

    fn member(n: u128, name: &str) -> GroupMember {
        GroupMember {
            person: Person {
                id: id(n),
                username: format!("user{n}"),
                display_name: name.into(),
                avatar_id: None,
                custom_status: None,
            },
            owner: n == 1,
            online: false,
        }
    }
    fn group(names: &[&str]) -> Group {
        Group {
            conversation_id: "00000000000000000000000000000099".parse().unwrap(),
            name: String::new(),
            revision: 1,
            members: names
                .iter()
                .enumerate()
                .map(|(i, n)| member(i as u128 + 1, n))
                .collect(),
            unread_count: 0,
        }
    }
    fn id(n: u128) -> AccountId {
        format!("{n:032x}").parse().unwrap()
    }
    fn me() -> Option<AccountId> {
        Some(id(1))
    }

    #[test]
    fn titles_prefer_names_then_other_members() {
        let mut g = group(&["Me", "Ada", "", "Cem"]);
        assert_eq!(title(&g, me(), Language::English), "Ada, user3, Cem");
        g.members.push(member(5, "Deniz"));
        assert_eq!(
            title(&g, me(), Language::English),
            "Ada, user3 and 2 others"
        );
        assert_eq!(
            title(&g, me(), Language::Turkish),
            "Ada, user3 ve 2 kişi daha"
        );
        g.name = "  Trip  ".into();
        assert_eq!(title(&g, me(), Language::English), "Trip");
        assert_eq!(title(&group(&["Me"]), me(), Language::English), "Just you");
        // Names are never translated.
        assert_eq!(
            title(&group(&["Me", "Settings"]), me(), Language::Turkish),
            "Settings"
        );
    }

    #[test]
    fn stacks_others_first_and_counts_members() {
        let g = group(&["Me", "Ada", "Bob", "Cem"]);
        let shown: Vec<_> = stack(&g, me()).into_iter().map(name).collect();
        assert_eq!(shown, ["Ada", "Bob"]);
        assert_eq!(stack(&group(&["Me"]), me()).len(), 1);
        assert_eq!(member_count(1, Language::English), "1 member");
        assert_eq!(member_count(3, Language::Turkish), "3 üye");
    }

    #[test]
    fn typing_and_system_lines() {
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(typing(&[], Language::English), None);
        assert_eq!(
            typing(&names(&["Ada"]), Language::English).unwrap(),
            "Ada is typing…"
        );
        assert_eq!(
            typing(&names(&["Ada", "Bob"]), Language::English).unwrap(),
            "Ada and Bob are typing…"
        );
        assert_eq!(
            typing(&names(&["A", "B", "C"]), Language::English).unwrap(),
            "Several people are typing…"
        );
        let named = |id: u128, n: &str| Named {
            id: super::tests::id(id),
            display_name: n.into(),
        };
        let line = |event| {
            system_text(
                &SystemMessage {
                    actor_name: "Ada".into(),
                    event,
                },
                Language::English,
            )
        };
        assert_eq!(line(SystemEvent::Created {}), "Ada created the group.");
        assert_eq!(
            line(SystemEvent::Added {
                people: vec![named(2, "Bob"), named(3, "Cem")]
            }),
            "Ada added Bob, Cem to the group."
        );
        assert_eq!(
            line(SystemEvent::Renamed {
                name: String::new()
            }),
            "Ada removed the group name."
        );
        assert_eq!(
            line(SystemEvent::OwnerChanged {
                person: named(2, "Bob")
            }),
            "Bob is now the group owner."
        );
        assert_eq!(
            line(SystemEvent::Removed {
                person: named(2, "Bob")
            }),
            "Ada removed Bob from the group."
        );
    }
}
