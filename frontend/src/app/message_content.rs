use super::Ui;
use crate::account_client;
use leptos::{either::Either, prelude::*};
use thiscord_shared::GuildId;

/// Mentions of listed guild members show their guild name (nickname or profile
/// name) in their role colour, with the `@username` as the tooltip. Other
/// mentions, and every mention outside guilds, show the text as written.
fn highlighted_mentions(text: &str, ui: Ui, guild: Option<GuildId>) -> impl IntoView + use<> {
    use thiscord_frontend::mentions::Span;
    thiscord_frontend::mentions::spans(text)
        .into_iter()
        .map(|span| match span {
            Span::Text(text) => Either::Left(text.to_owned()),
            Span::Mention(text) => {
                let member = guild.and_then(|g| ui.members.mention(g, &text[1..]));
                let colors = member
                    .as_ref()
                    .and_then(|m| super::members::mention_colors(m.color));
                let shown = member.map_or_else(|| text.to_owned(), |m| format!("@{}", m.name));
                Either::Right(view! {
                    <span class="mention-chip" title=text.to_owned()
                        style:color=colors.as_ref().map(|c| c.0.clone())
                        style:background-color=colors.map(|c| c.1)>{shown}</span>
                })
            }
        })
        .collect_view()
}

fn plain(text: &str, ui: Ui, guild: Option<GuildId>) -> impl IntoView + use<> {
    thiscord_frontend::message_links::spans(text)
        .into_iter()
        .map(|span| {
            let text = span.text.to_owned();
            match span.href {
                None => Either::Left(highlighted_mentions(&text, ui, guild)),
                Some(href) => {
                    let url = href.clone();
                    Either::Right(view! {
                        <a href=href target="_blank" rel="noopener noreferrer"
                            class="message-link"
                            on:click=move |event| {
                                if account_client::desktop() {
                                    event.prevent_default();
                                    let url = url.clone();
                                    leptos::task::spawn_local(async move {
                                        if let Err(error) = account_client::native::<()>(
                                            "open_external_url", serde_json::json!({ "url": url }),
                                        ).await {
                                            ui.status.set(error);
                                        }
                                    });
                                }
                            }
                        >{text}</a>
                    })
                }
            }
        })
        .collect_view()
}

/// Message markup; `guild` resolves mentions to that guild's members.
pub(super) fn render(text: &str, ui: Ui, guild: Option<GuildId>) -> impl IntoView + use<> {
    nodes(thiscord_frontend::markdown::parse(text), ui, guild, false)
}
fn nodes(
    items: Vec<thiscord_frontend::markdown::Node>,
    ui: Ui,
    guild: Option<GuildId>,
    literal: bool,
) -> AnyView {
    use thiscord_frontend::markdown::{Kind, Node};
    items.into_iter().map(|node| match node {
        Node::Text(text)|Node::Literal(text) if !literal => plain(&text,ui,guild).into_any(),
        Node::Text(text)|Node::Literal(text)=>text.into_any(),
        Node::Break=>view!{<br/>}.into_any(),
        Node::Element(kind,children)=>{
            let inner=nodes(children,ui,guild,literal || matches!(kind,Kind::Code|Kind::CodeBlock|Kind::Link(_)));
            match kind {
                Kind::Strong=>view!{<strong>{inner}</strong>}.into_any(),
                Kind::Emphasis=>view!{<em>{inner}</em>}.into_any(),
                Kind::Strike=>view!{<s>{inner}</s>}.into_any(),
                Kind::Code=>view!{<code class="inline-code">{inner}</code>}.into_any(),
                Kind::CodeBlock=>view!{<pre class="code-block"><code>{inner}</code></pre>}.into_any(),
                Kind::Quote=>view!{<blockquote class="message-quote">{inner}</blockquote>}.into_any(),
                Kind::List=>view!{<ul class="list-disc pl-6">{inner}</ul>}.into_any(),
                Kind::Item=>view!{<li>{inner}</li>}.into_any(),
                Kind::Paragraph=>view!{<div class="whitespace-pre-wrap break-words">{inner}</div>}.into_any(),
                Kind::Link(href)=>{
                    let url=href.clone();
                    view!{<a href=href target="_blank" rel="noopener noreferrer" class="message-link" on:click=move |event| {
                        if account_client::desktop() { event.prevent_default();let url=url.clone();leptos::task::spawn_local(async move {
                            if let Err(error)=account_client::native::<()>("open_external_url",serde_json::json!({"url":url})).await {ui.status.set(error);}
                        }); }
                    }>{inner}</a>}.into_any()
                }
                Kind::Plain=>inner,
            }
        }
    }).collect_view().into_any()
}
