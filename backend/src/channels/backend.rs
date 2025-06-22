#![allow(unused_imports)]
use crate::{
    Error,
    models::{Backend, Channel, NewChannel, PermissionType, Server},
    schema,
};
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
}
