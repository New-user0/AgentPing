//! Two-way remote interaction service (Discord bot).
//!
//! Hooks register interactions over the local API and long-poll for the answer; this service
//! posts interactive prompts, receives button presses and replies from the Discord Gateway,
//! authorizes the sender, resolves each interaction exactly once and keeps the Discord messages
//! in sync with what happened (answered remotely, handled in the terminal, expired).

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agentping_core::config::RemoteRoute;
use agentping_core::remote::{
    CreateInteraction, CreateInteractionResult, InteractionRequest, InteractionResponse,
    InteractionState,
};
use agentping_core::time::ago;
use agentping_core::Config;
use agentping_remote::discord::model::{Interaction, Message};
use agentping_remote::discord::prompt::{
    closed_prompt, describe_response, parse_custom_id, prompt_messages, ComponentAction,
};
use agentping_remote::discord::{DiscordApi, GatewayEvent};
use agentping_remote::reply::{self, Command};
use agentping_storage::{InteractionKind, InteractionRecord, InteractionStatus, NewInteraction};
use chrono::Utc;
use serde::Serialize;
use serde_json::json;
use tokio::sync::Notify;

use crate::delivery::{with_db, SharedDb};

const PRESENCE_KEY: &str = "remote.presence";
const LAST_BACK_KEY: &str = "remote.last_back";

/// Connection state shown by `agentping status` / `doctor`.
#[derive(Debug, Clone, Default, Serialize, serde::Deserialize)]
pub struct RemoteStatus {
    /// Remote interaction is configured.
    pub enabled: bool,
    /// Connected to the Discord Gateway.
    pub connected: bool,
    /// Fatal gateway error, if any.
    pub error: Option<String>,
    /// Whether prompts are currently routed (you are "away").
    pub away: bool,
    /// Pending interactions.
    pub pending: usize,
}

#[derive(Default)]
struct State {
    connected: bool,
    error: Option<String>,
}

/// The remote interaction service.
pub struct RemoteService {
    db: SharedDb,
    config: Arc<Config>,
    api: DiscordApi,
    channel_id: String,
    allowed: HashSet<String>,
    state: Mutex<State>,
    changed: Notify,
}

fn random_id() -> String {
    let bytes: [u8; 16] = rand::random();
    hex::encode(bytes)
}

fn kind_of(request: &InteractionRequest) -> InteractionKind {
    match request {
        InteractionRequest::Permission { .. } => InteractionKind::Permission,
        InteractionRequest::Question { .. } => InteractionKind::Question,
        InteractionRequest::Continue { .. } => InteractionKind::Continue,
    }
}

fn title_for(rec: &InteractionRecord) -> String {
    let project = rec
        .project_name
        .as_deref()
        .map(|p| format!(" · {p}"))
        .unwrap_or_default();
    match serde_json::from_value::<InteractionRequest>(rec.request.clone()) {
        Ok(InteractionRequest::Permission { tool_name, .. }) => {
            format!("🔐 Permission: {tool_name}{project}")
        }
        Ok(InteractionRequest::Question { .. }) => format!("💬 Question{project}"),
        _ => format!("↩️ Waiting for instructions{project}"),
    }
}

impl RemoteService {
    /// Creates the service.
    pub fn new(db: SharedDb, config: Arc<Config>, api: DiscordApi) -> anyhow::Result<Arc<Self>> {
        let channel_id = config
            .remote
            .discord_channel_id
            .clone()
            .filter(|c| !c.is_empty())
            .ok_or_else(|| anyhow::anyhow!("remote.discord_channel_id is not set"))?;
        let allowed = config.remote.discord_user_ids.iter().cloned().collect();
        Ok(Arc::new(Self {
            db,
            config,
            api,
            channel_id,
            allowed,
            state: Mutex::new(State::default()),
            changed: Notify::new(),
        }))
    }

    fn set_state(&self, f: impl FnOnce(&mut State)) {
        if let Ok(mut s) = self.state.lock() {
            f(&mut s);
        }
    }

    /// Current status.
    pub async fn status(&self) -> RemoteStatus {
        let (connected, error) = self
            .state
            .lock()
            .map(|s| (s.connected, s.error.clone()))
            .unwrap_or_default();
        let pending = with_db(&self.db, |db| {
            Ok(db.store().pending_interactions(Utc::now())?.len())
        })
        .await
        .unwrap_or(0);
        RemoteStatus {
            enabled: true,
            connected,
            error,
            away: self.is_away().await,
            pending,
        }
    }

    /// Whether prompts should be routed to Discord now.
    pub async fn is_away(&self) -> bool {
        let r = &self.config.remote;
        if r.route == RemoteRoute::Always {
            return true;
        }
        let threshold = i64::from(r.away_after_minutes);
        with_db(&self.db, move |db| {
            let store = db.store();
            if store.kv_get(PRESENCE_KEY)?.as_deref() == Some("away") {
                return Ok(true);
            }
            if threshold == 0 {
                return Ok(false);
            }
            let last_back = store
                .kv_get(LAST_BACK_KEY)?
                .as_deref()
                .and_then(agentping_core::time::parse_ts);
            let last = store.last_local_activity()?.max(last_back);
            Ok(match last {
                Some(t) => Utc::now() - t > chrono::Duration::minutes(threshold),
                None => true,
            })
        })
        .await
        .unwrap_or(false)
    }

    /// Sets presence explicitly (`away` routes prompts; `back` returns to automatic).
    ///
    /// Claude Code does not show its local dialog while a hook waits for a remote answer, so
    /// coming back also withdraws open permission requests and questions: their hooks stop
    /// waiting and the normal terminal prompt appears.
    pub async fn set_away(&self, away: bool) -> anyhow::Result<()> {
        let moved = with_db(&self.db, move |db| {
            let now = Utc::now();
            Ok(db.write(|s| {
                s.kv_set(PRESENCE_KEY, if away { "away" } else { "auto" }, now)?;
                let mut moved = Vec::new();
                if !away {
                    s.kv_set(LAST_BACK_KEY, &agentping_core::time::format_ts(now), now)?;
                    for i in s.pending_interactions(now)? {
                        if i.kind == InteractionKind::Continue {
                            continue;
                        }
                        if let Some(r) = s.close_interaction(
                            &i.id,
                            InteractionStatus::Cancelled,
                            "moved back to the terminal",
                            now,
                        )? {
                            moved.push(r);
                        }
                    }
                }
                Ok(moved)
            })?)
        })
        .await?;
        if !moved.is_empty() {
            self.changed.notify_waiters();
            for rec in &moved {
                self.mark_closed(rec, "💻 Moved back to the terminal").await;
            }
        }
        Ok(())
    }

    fn connected(&self) -> bool {
        self.state.lock().map(|s| s.connected).unwrap_or(false)
    }

    /// Registers an interaction for a waiting hook and posts the prompt.
    pub async fn create(&self, req: CreateInteraction) -> anyhow::Result<CreateInteractionResult> {
        let r = &self.config.remote;
        let kind = kind_of(&req.request);
        let enabled = match kind {
            InteractionKind::Permission => r.permissions,
            InteractionKind::Question => r.questions,
            InteractionKind::Continue => r.continue_replies,
        };
        let not_routed = |reason: &str| CreateInteractionResult {
            routed: false,
            id: None,
            reason: Some(reason.to_owned()),
        };
        if !enabled {
            return Ok(not_routed("disabled in [remote]"));
        }
        if !self.connected() {
            return Ok(not_routed("the Discord bot is not connected"));
        }
        if kind != InteractionKind::Continue && !self.is_away().await {
            return Ok(not_routed("you are at the keyboard (presence: back)"));
        }
        let id = random_id();
        let timeout = req.timeout_secs.clamp(10, 24 * 3600);
        let request = serde_json::to_value(&req.request)?;
        {
            let id = id.clone();
            let req = req.clone();
            let request = request.clone();
            let superseded = with_db(&self.db, move |db| {
                let now = Utc::now();
                Ok(db.write(|s| {
                    let mut superseded = Vec::new();
                    if let Some(key) = req.supersedes_cancel_key.as_deref() {
                        s.cancel_pending(key, "sent as an interactive Discord prompt", now)?;
                    }
                    if let (InteractionKind::Continue, Some(session)) =
                        (kind, req.session_id.as_deref())
                    {
                        superseded = s.close_session_interactions(
                            &req.agent,
                            session,
                            Some(InteractionKind::Continue),
                            "superseded by a newer turn",
                            now,
                        )?;
                    }
                    s.create_interaction(
                        &NewInteraction {
                            id: &id,
                            kind,
                            agent: &req.agent,
                            session_id: req.session_id.as_deref(),
                            project_name: req.project_name.as_deref(),
                            request: &request,
                            expires_at: now + chrono::Duration::seconds(timeout as i64),
                        },
                        now,
                    )?;
                    Ok(superseded)
                })?)
            })
            .await?;
            if !superseded.is_empty() {
                self.changed.notify_waiters();
            }
        }
        if kind != InteractionKind::Continue {
            let messages = prompt_messages(
                &id,
                &req.request,
                &req.agent_name,
                req.project_name.as_deref(),
                timeout.div_ceil(60),
            );
            let mut last_id = None;
            for m in &messages {
                match self.api.send_message(&self.channel_id, m).await {
                    Ok(msg) => last_id = Some(msg.id),
                    Err(e) => {
                        tracing::error!(error = %e, "could not post remote prompt");
                        let id2 = id.clone();
                        let _ = with_db(&self.db, move |db| {
                            Ok(db.write(|s| {
                                s.close_interaction(
                                    &id2,
                                    InteractionStatus::Cancelled,
                                    "could not post to Discord",
                                    Utc::now(),
                                )
                            })?)
                        })
                        .await;
                        return Ok(not_routed("could not post to Discord"));
                    }
                }
            }
            if let Some(mid) = last_id {
                let id2 = id.clone();
                with_db(&self.db, move |db| {
                    Ok(db.write(|s| s.set_interaction_message(&id2, &mid))?)
                })
                .await?;
            }
        }
        tracing::info!(interaction = %id, kind = %kind, "remote interaction opened");
        Ok(CreateInteractionResult {
            routed: true,
            id: Some(id),
            reason: None,
        })
    }

    async fn state_of(&self, id: &str) -> anyhow::Result<Option<InteractionRecord>> {
        let id = id.to_owned();
        with_db(&self.db, move |db| Ok(db.store().interaction(&id)?)).await
    }

    /// Waits up to `timeout` for an interaction to leave the pending state.
    pub async fn wait(
        &self,
        id: &str,
        timeout: Duration,
    ) -> anyhow::Result<Option<InteractionState>> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let Some(rec) = self.state_of(id).await? else {
                return Ok(None);
            };
            let expired = rec.status == InteractionStatus::Pending && rec.expires_at <= Utc::now();
            if rec.status != InteractionStatus::Pending || expired {
                let response = rec
                    .response
                    .clone()
                    .filter(|_| rec.status == InteractionStatus::Resolved)
                    .and_then(|v| serde_json::from_value::<InteractionResponse>(v).ok());
                return Ok(Some(InteractionState {
                    status: if expired {
                        "expired".into()
                    } else {
                        rec.status.as_str().into()
                    },
                    response,
                    responder: rec.responder,
                }));
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Ok(Some(InteractionState {
                    status: "pending".into(),
                    response: None,
                    responder: None,
                }));
            }
        }
    }

    /// Withdraws an interaction (the hook stopped waiting).
    pub async fn cancel(&self, id: &str, note: &str) -> anyhow::Result<()> {
        let (id, note2) = (id.to_owned(), note.to_owned());
        let closed = with_db(&self.db, move |db| {
            Ok(db.write(|s| {
                s.close_interaction(&id, InteractionStatus::Cancelled, &note2, Utc::now())
            })?)
        })
        .await?;
        if let Some(rec) = closed {
            self.changed.notify_waiters();
            self.mark_closed(&rec, &format!("⏹️ {note}")).await;
        }
        Ok(())
    }

    /// Updates Discord for interactions withdrawn by local activity.
    pub async fn on_closed(&self, records: Vec<InteractionRecord>) {
        if records.is_empty() {
            return;
        }
        self.changed.notify_waiters();
        for rec in records {
            self.mark_closed(&rec, "⏹️ Handled in the terminal").await;
        }
    }

    async fn mark_closed(&self, rec: &InteractionRecord, outcome: &str) {
        if let Some(mid) = rec.remote_message_id.as_deref() {
            let body = closed_prompt(&title_for(rec), outcome, 0x95A5A6);
            if let Err(e) = self.api.edit_message(&self.channel_id, mid, &body).await {
                tracing::debug!(error = %e, "could not update remote prompt");
            }
        }
    }

    /// Expires overdue interactions (run periodically).
    pub async fn sweep(&self) {
        let expired = with_db(&self.db, |db| {
            Ok(db.write(|s| s.expire_interactions(Utc::now()))?)
        })
        .await
        .unwrap_or_default();
        if !expired.is_empty() {
            self.changed.notify_waiters();
        }
        for rec in expired {
            self.mark_closed(&rec, "⌛ Expired — answer it in the terminal")
                .await;
        }
    }

    /// Handles one gateway event.
    pub async fn on_event(&self, event: GatewayEvent) {
        match event {
            GatewayEvent::Ready { .. } => self.set_state(|s| {
                s.connected = true;
                s.error = None;
            }),
            GatewayEvent::Fatal(reason) => self.set_state(|s| {
                s.connected = false;
                s.error = Some(reason);
            }),
            GatewayEvent::MessageCreate(m) => {
                if let Err(e) = self.on_message(&m).await {
                    tracing::error!(error = %e, "failed to handle Discord message");
                }
            }
            GatewayEvent::Interaction(i) => {
                if let Err(e) = self.on_interaction(&i).await {
                    tracing::error!(error = %e, "failed to handle Discord interaction");
                }
            }
        }
    }

    async fn say(&self, text: &str, reply_to: Option<&str>) {
        let mut body = json!({ "content": text, "allowed_mentions": { "parse": [] } });
        if let Some(r) = reply_to {
            body["message_reference"] = json!({ "message_id": r, "fail_if_not_exists": false });
        }
        if let Err(e) = self.api.send_message(&self.channel_id, &body).await {
            tracing::debug!(error = %e, "could not send Discord message");
        }
    }

    async fn on_message(&self, m: &Message) -> anyhow::Result<()> {
        if m.author.bot || m.webhook_id.is_some() || m.channel_id != self.channel_id {
            return Ok(());
        }
        if !self.allowed.contains(&m.author.id) {
            tracing::warn!(user = %m.author.id, "ignoring Discord message from a user who is not allow-listed");
            return Ok(());
        }
        let text = m.content.trim();
        if text.is_empty() {
            return Ok(());
        }
        if let Some(cmd) = reply::parse_command(text) {
            return self.command(cmd, &m.id).await;
        }
        let reference = m
            .message_reference
            .as_ref()
            .and_then(|r| r.message_id.clone());
        let Some(target) = self.target_for(reference.as_deref()).await? else {
            let hint = if reference.is_some() {
                "That session isn't waiting for anything right now. (A finished session listens for replies for a while; prompts expire.)"
            } else {
                "Nothing is waiting for an answer right now. Send `status` to see sessions or `help` for commands."
            };
            self.say(hint, Some(&m.id)).await;
            return Ok(());
        };
        let request: InteractionRequest = serde_json::from_value(target.request.clone())?;
        let partial = partial_answers(&target);
        let Some(response) = reply::interpret(&request, &partial, text) else {
            return Ok(());
        };
        let responder = format!("{} ({})", m.author.display(), m.author.id);
        if let (
            InteractionRequest::Question { questions },
            InteractionResponse::Question { answers },
        ) = (&request, &response)
        {
            if !reply::all_answered(questions, answers) {
                let (id, v) = (target.id.clone(), serde_json::to_value(&response)?);
                with_db(&self.db, move |db| {
                    Ok(db.write(|s| s.set_interaction_progress(&id, &v))?)
                })
                .await?;
                let left = questions.len() - answers.len();
                self.say(
                    &format!("Got it. {left} more question(s) to answer."),
                    Some(&m.id),
                )
                .await;
                return Ok(());
            }
        }
        if self.resolve(&target, &response, &responder).await? {
            let _ = self.api.react(&self.channel_id, &m.id, "✅").await;
        } else {
            self.say(
                "Too late: that prompt was already answered, withdrawn or expired.",
                Some(&m.id),
            )
            .await;
        }
        Ok(())
    }

    /// Picks the interaction a reply is meant for.
    async fn target_for(
        &self,
        reference: Option<&str>,
    ) -> anyhow::Result<Option<InteractionRecord>> {
        let reference = reference.map(str::to_owned);
        with_db(&self.db, move |db| {
            let store = db.store();
            let now = Utc::now();
            let pending = store.pending_interactions(now)?;
            let Some(r) = reference else {
                return Ok(pending.into_iter().next());
            };
            if let Some(i) = store.interaction_by_message(&r)? {
                return Ok(
                    (i.status == InteractionStatus::Pending && i.expires_at > now).then_some(i),
                );
            }
            let Some((agent, session)) = store.session_for_message("discord", &r)? else {
                return Ok(None);
            };
            let mut mine: Vec<_> = pending
                .into_iter()
                .filter(|i| i.agent == agent && i.session_id.as_deref() == Some(session.as_str()))
                .collect();
            // Prefer an open permission request or question over the continue listener.
            mine.sort_by_key(|i| i.kind == InteractionKind::Continue);
            Ok(mine.into_iter().next())
        })
        .await
    }

    async fn resolve(
        &self,
        target: &InteractionRecord,
        response: &InteractionResponse,
        responder: &str,
    ) -> anyhow::Result<bool> {
        let (id, v, who) = (
            target.id.clone(),
            serde_json::to_value(response)?,
            responder.to_owned(),
        );
        let ok = with_db(&self.db, move |db| {
            Ok(db.write(|s| s.resolve_interaction(&id, &v, &who, Utc::now()))?)
        })
        .await?;
        if ok {
            self.changed.notify_waiters();
            tracing::info!(interaction = %target.id, kind = %target.kind, responder, "remote interaction answered");
            let outcome = format!("{}\n_by {responder}_", describe_response(response));
            self.mark_closed(target, &outcome).await;
        }
        Ok(ok)
    }

    async fn command(&self, cmd: Command, reply_to: &str) -> anyhow::Result<()> {
        match cmd {
            Command::Help => {
                self.say(
                    "**AgentPing commands**\n\
                     • Reply to a 🔐 prompt with `approve`, `deny`, or a reason; or tap the buttons\n\
                     • Reply to a 💬 question with an option number, a label, or your own answer\n\
                     • Reply to a ✅ finished / ⏳ waiting ping with new instructions for Claude\n\
                     • `status`: sessions and open prompts · `away` / `back`: route prompts here or not",
                    Some(reply_to),
                )
                .await;
            }
            Command::Away => {
                self.set_away(true).await?;
                self.say(
                    "🚶 Away mode on: permission requests and questions will come here.",
                    Some(reply_to),
                )
                .await;
            }
            Command::Back => {
                self.set_away(false).await?;
                self.say(
                    "👋 Welcome back: prompts stay in the terminal.",
                    Some(reply_to),
                )
                .await;
            }
            Command::Status => {
                let text = self.status_text().await?;
                self.say(&text, Some(reply_to)).await;
            }
        }
        Ok(())
    }

    async fn status_text(&self) -> anyhow::Result<String> {
        let away = self.is_away().await;
        let (sessions, pending) = with_db(&self.db, |db| {
            let now = Utc::now();
            Ok((
                db.store()
                    .sessions(now - chrono::Duration::hours(24), false)?,
                db.store().pending_interactions(now)?,
            ))
        })
        .await?;
        let now = Utc::now();
        let mut out = format!(
            "**AgentPing** · presence: {}\n",
            if away {
                "away 🚶"
            } else {
                "at keyboard 💻"
            }
        );
        if sessions.is_empty() {
            out.push_str("No active sessions.\n");
        }
        for s in sessions.iter().take(10) {
            out.push_str(&format!(
                "• **{}** — {} ({})\n",
                s.project_name.as_deref().unwrap_or("unknown"),
                s.status.label(),
                ago(s.last_activity_at, now)
            ));
        }
        if !pending.is_empty() {
            out.push_str(&format!("{} open prompt(s).", pending.len()));
        }
        Ok(out)
    }

    async fn on_interaction(&self, i: &Interaction) -> anyhow::Result<()> {
        if i.kind != 3 {
            return Ok(());
        }
        let ephemeral = |text: &str| json!({ "type": 4, "data": { "content": text, "flags": 64 } });
        let Some(actor) = i.actor() else {
            return Ok(());
        };
        if !self.allowed.contains(&actor.id) {
            tracing::warn!(user = %actor.id, "rejected Discord button press from a user who is not allow-listed");
            self.api
                .interaction_response(
                    &i.id,
                    &i.token,
                    &ephemeral("You are not allowed to answer AgentPing prompts."),
                )
                .await?;
            return Ok(());
        }
        let data = i.data.clone().unwrap_or_default();
        let Some((interaction_id, action)) = parse_custom_id(&data.custom_id) else {
            return Ok(());
        };
        let Some(target) = self.state_of(&interaction_id).await? else {
            return Ok(());
        };
        if target.status != InteractionStatus::Pending || target.expires_at <= Utc::now() {
            self.api
                .interaction_response(
                    &i.id,
                    &i.token,
                    &ephemeral("This prompt was already answered, withdrawn or expired."),
                )
                .await?;
            return Ok(());
        }
        let request: InteractionRequest = serde_json::from_value(target.request.clone())?;
        let responder = format!("{} ({})", actor.display(), actor.id);
        let response = match (&request, action) {
            (InteractionRequest::Permission { .. }, ComponentAction::Allow) => {
                InteractionResponse::Permission {
                    allow: true,
                    message: None,
                }
            }
            (InteractionRequest::Permission { .. }, ComponentAction::Deny) => {
                InteractionResponse::Permission {
                    allow: false,
                    message: None,
                }
            }
            (
                InteractionRequest::Question { questions },
                ComponentAction::Option { question, option },
            ) => {
                let mut answers = partial_answers(&target);
                let Some(q) = questions.get(question) else {
                    return Ok(());
                };
                let Some(o) = q.options.get(option) else {
                    return Ok(());
                };
                answers.insert(q.question.clone(), o.label.clone());
                InteractionResponse::Question { answers }
            }
            (InteractionRequest::Question { questions }, ComponentAction::Select { question }) => {
                let mut answers = partial_answers(&target);
                let Some(q) = questions.get(question) else {
                    return Ok(());
                };
                let labels: Vec<String> = data
                    .values
                    .iter()
                    .filter_map(|v| v.parse::<usize>().ok())
                    .filter_map(|idx| q.options.get(idx).map(|o| o.label.clone()))
                    .collect();
                answers.insert(q.question.clone(), labels.join(", "));
                InteractionResponse::Question { answers }
            }
            _ => return Ok(()),
        };
        if let (
            InteractionRequest::Question { questions },
            InteractionResponse::Question { answers },
        ) = (&request, &response)
        {
            if !reply::all_answered(questions, answers) {
                let (id, v) = (target.id.clone(), serde_json::to_value(&response)?);
                with_db(&self.db, move |db| {
                    Ok(db.write(|s| s.set_interaction_progress(&id, &v))?)
                })
                .await?;
                let left = questions.len() - answers.len();
                self.api
                    .interaction_response(
                        &i.id,
                        &i.token,
                        &ephemeral(&format!("Recorded. {left} more question(s) to answer.")),
                    )
                    .await?;
                return Ok(());
            }
        }
        let (id, v, who) = (
            target.id.clone(),
            serde_json::to_value(&response)?,
            responder.clone(),
        );
        let ok = with_db(&self.db, move |db| {
            Ok(db.write(|s| s.resolve_interaction(&id, &v, &who, Utc::now()))?)
        })
        .await?;
        if !ok {
            self.api
                .interaction_response(
                    &i.id,
                    &i.token,
                    &ephemeral("Too late: this prompt is no longer open."),
                )
                .await?;
            return Ok(());
        }
        self.changed.notify_waiters();
        tracing::info!(interaction = %target.id, kind = %target.kind, responder = %responder, "remote interaction answered");
        let outcome = format!("{}\n_by {responder}_", describe_response(&response));
        let mut update = closed_prompt(&title_for(&target), &outcome, 0x2ECC71);
        let update = json!({ "type": 7, "data": update.take() });
        self.api
            .interaction_response(&i.id, &i.token, &update)
            .await?;
        Ok(())
    }
}

fn partial_answers(rec: &InteractionRecord) -> BTreeMap<String, String> {
    rec.response
        .as_ref()
        .and_then(|v| serde_json::from_value::<InteractionResponse>(v.clone()).ok())
        .and_then(|r| match r {
            InteractionResponse::Question { answers } => Some(answers),
            _ => None,
        })
        .unwrap_or_default()
}

/// Starts the gateway and event loop. Returns the service.
pub fn start(
    db: SharedDb,
    config: Arc<Config>,
    api: DiscordApi,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<Arc<RemoteService>> {
    let service = RemoteService::new(db, config, api.clone())?;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<GatewayEvent>(64);
    tokio::spawn(agentping_remote::discord::gateway::run(
        api,
        tx,
        shutdown.clone(),
    ));
    let svc = service.clone();
    tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            svc.on_event(ev).await;
        }
    });
    let svc = service.clone();
    let mut stop = shutdown;
    tokio::spawn(async move {
        loop {
            let stopping = async {
                let _ = stop.wait_for(|s| *s).await;
            };
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(15)) => {}
                _ = stopping => break,
            }
            svc.sweep().await;
        }
    });
    Ok(service)
}
