mod err;
mod gmail;
pub mod images;

use std::collections::HashSet;

pub use err::Error;
pub use gmail::GmailBackend;
use shared::WebSocketMessage;

use crate::models::user::OnlineUser;

pub trait SubscribableOnce {
    fn subscribe(&self, user: &OnlineUser);
    fn unsubscribe(user: &OnlineUser);
    fn get_subscribers(&self) -> HashSet<OnlineUser>;
    fn get_subscribed(user: &OnlineUser) -> Option<Self> where Self: Sized;
    fn notify_subscribers(&self, message: WebSocketMessage) -> impl std::future::Future<Output = ()> {async move {
        let message = message;
        for user in self.get_subscribers() {
            user.websocket.clone().send(message.clone()).await.unwrap_or_else(|e| {
                tracing::error!("Failed to send message to user {}: {}", user.user.id, e);
            });
        }
    } }
}