#![allow(unused_imports)]
use crate::{
    channels::{VoiceRooms, VOICE_ROOMS}, models::Backend, servers::UsersActiveServers, Error
};
use shared::{models::{Channel, ChannelType, ChannelWithUsers, NewChannel, PermissionType, Server, VoiceUser}, schema};
use diesel::prelude::*;
use rand::{Rng, distr::Alphanumeric};
use strum::IntoEnumIterator;
use uuid::Uuid;

impl Backend {
    pub fn get_channel(&self, server_id: Uuid, channel_id: Uuid) -> Result<Option<Channel>, Error> {
        let mut conn = self.get_connection()?;
        let channel = schema::channels::dsl::channels
            .filter(schema::channels::dsl::server_id.eq(server_id))
            .filter(schema::channels::dsl::id.eq(channel_id))
            .first::<Channel>(&mut conn)
            .optional()?;
        Ok(channel)
    }

    pub fn list_channels(&self, server_id: Uuid) -> Result<Vec<Channel>, Error> {
        let mut conn = self.get_connection()?;
        let channels = schema::channels::dsl::channels
            .filter(schema::channels::dsl::server_id.eq(server_id))
            .load::<Channel>(&mut conn)?;
        Ok(channels)
    }

    pub fn create_channel(&self, new_channel: &NewChannel) -> Result<Channel, Error> {
        let mut conn = self.get_connection()?;
        let channel = diesel::insert_into(schema::channels::dsl::channels)
            .values(new_channel)
            .get_result::<Channel>(&mut conn)?;
        Ok(channel)
    }

    pub async fn convert_channel_to_with_users(
        channel: Channel,
    ) -> ChannelWithUsers {
        let mut users = vec![];
        let rooms = VoiceRooms::get_or_init();
        if channel.type_ == ChannelType::Voice {
            let room = rooms.get_room_or_init(channel.id);
            {
                let people = room.people.lock().await;
                for person in people.iter() {
                    if let Some(user_id) = person.id {
                        users.push(VoiceUser {
                            id: user_id,
                            username: person.name.clone().unwrap(),
                        });
                    }
                }

            }
        }
        ChannelWithUsers {
            channel,
            users,
        }
    }
}