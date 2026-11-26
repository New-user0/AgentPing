//! The subset of Discord API objects AgentPing uses. Unknown fields are ignored.

use serde::Deserialize;

/// A Discord user.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct User {
    /// Snowflake id.
    pub id: String,
    /// Username.
    #[serde(default)]
    pub username: String,
    /// Display name.
    #[serde(default)]
    pub global_name: Option<String>,
    /// Whether this is a bot account.
    #[serde(default)]
    pub bot: bool,
}

impl User {
    /// Name to show in acknowledgements.
    pub fn display(&self) -> &str {
        self.global_name.as_deref().unwrap_or(&self.username)
    }
}

/// Reference to another message (Discord "reply").
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct MessageReference {
    /// Referenced message id.
    #[serde(default)]
    pub message_id: Option<String>,
}

/// A message.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Message {
    /// Snowflake id.
    pub id: String,
    /// Channel id.
    pub channel_id: String,
    /// Author.
    pub author: User,
    /// Text content (empty without the Message Content intent).
    #[serde(default)]
    pub content: String,
    /// Reply target.
    #[serde(default)]
    pub message_reference: Option<MessageReference>,
    /// Webhook id when posted by a webhook.
    #[serde(default)]
    pub webhook_id: Option<String>,
}

/// A channel.
#[derive(Debug, Clone, Deserialize)]
pub struct Channel {
    /// Snowflake id.
    pub id: String,
    /// Name.
    #[serde(default)]
    pub name: Option<String>,
    /// Guild id.
    #[serde(default)]
    pub guild_id: Option<String>,
}

/// Component interaction data.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct InteractionData {
    /// Developer-defined id of the component.
    #[serde(default)]
    pub custom_id: String,
    /// Selected values (select menus).
    #[serde(default)]
    pub values: Vec<String>,
}

/// Guild member wrapper.
#[derive(Debug, Clone, Deserialize)]
pub struct Member {
    /// The user.
    pub user: User,
}

/// The message an interaction belongs to.
#[derive(Debug, Clone, Deserialize)]
pub struct InteractionMessage {
    /// Message id.
    pub id: String,
}

/// An interaction (button press, select menu).
#[derive(Debug, Clone, Deserialize)]
pub struct Interaction {
    /// Interaction id.
    pub id: String,
    /// Continuation token for the response.
    pub token: String,
    /// Interaction type (3 = message component).
    #[serde(rename = "type")]
    pub kind: u8,
    /// Component data.
    #[serde(default)]
    pub data: Option<InteractionData>,
    /// Member (in guilds).
    #[serde(default)]
    pub member: Option<Member>,
    /// User (in DMs).
    #[serde(default)]
    pub user: Option<User>,
    /// Channel id.
    #[serde(default)]
    pub channel_id: Option<String>,
    /// Source message.
    #[serde(default)]
    pub message: Option<InteractionMessage>,
}

impl Interaction {
    /// The user who triggered the interaction.
    pub fn actor(&self) -> Option<&User> {
        self.member.as_ref().map(|m| &m.user).or(self.user.as_ref())
    }
}
