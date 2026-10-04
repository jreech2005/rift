//! Phase 2 runtime: the one place where a player action meets the NPC,
//! narrative and Director systems.
//!
//! ```text
//! player_action ─► session (action::apply) ─► NPC perception ─► Narrative ─┐   sync,
//!        ▲            acknowledgement + narrative consequences ◄───────────┘   deterministic
//!        │
//!        └─ world_event ◄─ validated apply ◄─ Director (once) ◄─ DirectorContext   async
//!                                                               (updated state)
//! ```
//!
//! * [`Runtime::apply_player_action`] is synchronous and deterministic: no
//!   model, no database. It either applies the whole action or nothing.
//! * [`Runtime::complete`] does the slow part afterwards: it persists NPC
//!   memories and, when the action was meaningful, asks the Director once and
//!   applies its decision after revalidating it against the state as it is
//!   then. What a decision produces never triggers another decision.
//!
//! The runtime owns no session model of its own. It drives the existing
//! [`SessionStore`], [`NpcDirectory`], [`MemoryStore`] and [`DirectorEngine`],
//! and keeps per session only what had no home yet: the [`NarrativeState`].
//! A session without a world takes the plain protocol V1 path unchanged.
//! See `docs/RUNTIME.md`.

mod adapt;
mod apply;
mod view;
mod world;

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::action::{self, ValidatedAction, WorldEvent, WorldEventType};
use crate::director::{
    DirectorContext, DirectorDecision, DirectorEngine, DirectorError, Trigger, validate_actions,
};
use crate::narrative::{
    CheckpointChange, Effect, MissionChange, MissionStatus, NarrativeEngine, NarrativeError,
    NarrativeState, ObjectiveChange, ObjectiveStatus, ReplanRequest,
};
use crate::npc::events::Roster;
use crate::npc::{
    CharacterId, InMemoryMemoryStore, LocationId, MemoryEntry, MemoryStore, NpcDirectory, NpcError,
    StoreError,
};
use crate::protocol::{ErrorCode, ProtocolError};
use crate::session::{GameSession, SessionStore};
use crate::voice::VoiceService;

pub use apply::{ApplyError, ReplanNote};
pub use world::{MAX_SECRET_KEYWORDS, MAX_SECRETS, RuntimeWorld, Scenario, Secret, WorldError};

/// Number of Director replan requests kept per session.
pub const REPLAN_NOTES_CAP: usize = 16;

/// Why a player action was rejected. Nothing was changed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error("npc layer rejected the action: {0}")]
    Npc(#[from] NpcError),
    #[error("narrative layer rejected the action: {0}")]
    Narrative(#[from] NarrativeError),
}

impl RuntimeError {
    /// The protocol V1 error to send to the client.
    pub fn to_protocol(&self) -> ProtocolError {
        match self {
            Self::Protocol(err) => err.clone(),
            other => ProtocolError::new(ErrorCode::InvalidAction, other.to_string()),
        }
    }
}

/// The deterministic result of one accepted player action.
#[derive(Debug, Clone)]
pub struct Applied {
    /// The acknowledgement: what `action::apply` produced, as before Phase 2.
    pub event: WorldEvent,
    /// Narrative consequences of the action, already recorded in the session.
    pub consequences: Vec<WorldEvent>,
    /// Set when the narrative plan can no longer proceed as written.
    pub replan: Option<ReplanRequest>,
    /// Slow work still owed for this action. Hand it to [`Runtime::complete`].
    pub follow_up: Option<FollowUp>,
}

/// Work for [`Runtime::complete`]: memories to persist and, for a meaningful
/// action, the Director's view of the state the action left behind.
#[derive(Debug, Clone)]
pub struct FollowUp {
    pub session_id: Uuid,
    pub memories: Vec<MemoryEntry>,
    pub context: Option<Box<DirectorContext>>,
}

/// What the Director did about one player action.
#[derive(Debug, Clone)]
pub enum DirectorReport {
    /// The action was not meaningful enough to ask.
    NotInvoked,
    /// Every action of the decision was applied. `deferred` lists the replan
    /// requests that were recorded but not executed.
    Applied {
        decision: Box<DirectorDecision>,
        deferred: Vec<ReplanNote>,
    },
    /// The decision was not applied; nothing changed.
    Rejected {
        decision: Box<DirectorDecision>,
        error: ApplyError,
    },
    /// No decision: the provider and its fallback failed. Nothing changed.
    Failed(DirectorError),
}

impl DirectorReport {
    pub fn decision(&self) -> Option<&DirectorDecision> {
        match self {
            Self::Applied { decision, .. } | Self::Rejected { decision, .. } => Some(decision),
            _ => None,
        }
    }

    pub fn is_applied(&self) -> bool {
        matches!(self, Self::Applied { .. })
    }
}

#[derive(Debug, Clone)]
pub struct FollowUpOutcome {
    /// World events produced by the applied decision, already recorded.
    pub events: Vec<WorldEvent>,
    pub director: DirectorReport,
    pub memories_stored: usize,
    /// Memories the store refused. NPC state is authoritative in memory, so
    /// this is lost recall, not lost state.
    pub memory_errors: Vec<StoreError>,
}

/// [`Applied`] and its [`FollowUpOutcome`] together.
#[derive(Debug, Clone)]
pub struct RuntimeOutcome {
    pub event: WorldEvent,
    pub consequences: Vec<WorldEvent>,
    pub replan: Option<ReplanRequest>,
    pub follow_up: Option<FollowUpOutcome>,
}

impl RuntimeOutcome {
    /// Every world event the action caused, in session order.
    pub fn events(&self) -> impl Iterator<Item = &WorldEvent> {
        std::iter::once(&self.event)
            .chain(&self.consequences)
            .chain(self.follow_up.iter().flat_map(|f| &f.events))
    }

    pub fn director(&self) -> &DirectorReport {
        self.follow_up
            .as_ref()
            .map_or(&DirectorReport::NotInvoked, |f| &f.director)
    }
}

/// The part of a session's state that only the runtime holds.
struct Story {
    narrative: NarrativeState,
    /// NPCs currently on stage.
    active_npcs: BTreeSet<String>,
    replans: VecDeque<ReplanNote>,
}

/// Cheap to clone; clones share everything.
#[derive(Clone)]
pub struct Runtime {
    sessions: SessionStore,
    npcs: NpcDirectory,
    memory: Arc<dyn MemoryStore>,
    director: DirectorEngine,
    world: Option<Arc<RuntimeWorld>>,
    /// Optional NPC voice. Never needed for a decision to apply.
    voice: Option<VoiceService>,
    /// One lock per session serialises every runtime write to it. Held for
    /// short synchronous sections only, never across an `.await`.
    stories: Arc<Mutex<HashMap<Uuid, Arc<Mutex<Story>>>>>,
}

impl fmt::Debug for Runtime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Runtime")
            .field("universe_id", &self.world.as_ref().map(|w| w.universe_id()))
            .finish_non_exhaustive()
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new(SessionStore::new())
    }
}

impl Runtime {
    /// A runtime with no world, the deterministic Director and an in-memory
    /// memory store. Without a world every session takes the plain path.
    pub fn new(sessions: SessionStore) -> Self {
        Self {
            sessions,
            npcs: NpcDirectory::new(),
            memory: Arc::new(InMemoryMemoryStore::new()),
            director: DirectorEngine::deterministic(),
            world: None,
            voice: None,
            stories: Arc::default(),
        }
    }

    /// Serve this world: sessions created from now on run its story.
    pub fn with_world(mut self, world: RuntimeWorld) -> Self {
        self.world = Some(Arc::new(world));
        self
    }

    pub fn with_director(mut self, director: DirectorEngine) -> Self {
        self.director = director;
        self
    }

    pub fn with_memory_store(mut self, memory: Arc<dyn MemoryStore>) -> Self {
        self.memory = memory;
        self
    }

    /// Speak NPC dialogue: `dialogue_started` events gain an `audio_url`
    /// when synthesis succeeds.
    pub fn with_voice(mut self, voice: VoiceService) -> Self {
        self.voice = Some(voice);
        self
    }

    pub fn voice(&self) -> Option<&VoiceService> {
        self.voice.as_ref()
    }

    pub fn sessions(&self) -> &SessionStore {
        &self.sessions
    }

    pub fn npcs(&self) -> &NpcDirectory {
        &self.npcs
    }

    pub fn memory_store(&self) -> &Arc<dyn MemoryStore> {
        &self.memory
    }

    pub fn world(&self) -> Option<&RuntimeWorld> {
        self.world.as_deref()
    }

    /// Snapshot of a session's narrative state, if it runs a world.
    pub fn narrative(&self, session_id: Uuid) -> Option<NarrativeState> {
        let story = self.story(session_id)?;
        let story = story.lock().expect("story lock poisoned");
        Some(story.narrative.clone())
    }

    /// NPCs currently on stage in a session.
    pub fn active_npcs(&self, session_id: Uuid) -> BTreeSet<String> {
        self.story(session_id).map_or_else(BTreeSet::new, |story| {
            story
                .lock()
                .expect("story lock poisoned")
                .active_npcs
                .clone()
        })
    }

    /// Replan requests the Director issued for a session, oldest first.
    pub fn replan_notes(&self, session_id: Uuid) -> Vec<ReplanNote> {
        self.story(session_id).map_or_else(Vec::new, |story| {
            let story = story.lock().expect("story lock poisoned");
            story.replans.iter().cloned().collect()
        })
    }

    fn story(&self, session_id: Uuid) -> Option<Arc<Mutex<Story>>> {
        self.stories
            .lock()
            .expect("runtime story map lock poisoned")
            .get(&session_id)
            .cloned()
    }

    fn roster(&self, session_id: Uuid) -> Roster {
        self.npcs
            .list(session_id)
            .into_iter()
            .map(|state| (state.character_id().clone(), state))
            .collect()
    }

    /// Create a session. When a world is configured the session starts its
    /// story: NPCs seeded, narrative plan started, player placed.
    pub fn create_session(&self) -> GameSession {
        let mut session = self.sessions.create();
        let Some(world) = self.world.as_deref() else {
            return session;
        };
        let session_id = session.session_id;
        match self.start_story(world, session_id) {
            Ok(story) => {
                session.current_location = story.narrative.world.player_location.clone();
                self.sessions.replace(session.clone());
                self.stories
                    .lock()
                    .expect("runtime story map lock poisoned")
                    .insert(session_id, Arc::new(Mutex::new(story)));
                info!(%session_id, universe_id = %world.universe_id, "story started");
            }
            Err(err) => {
                // The world was checked at load, so this is not expected. The
                // session stays usable on the plain path.
                error!(%session_id, error = %err, "could not start story; session has no world");
                self.npcs.remove_session(session_id);
            }
        }
        session
    }

    fn start_story(&self, world: &RuntimeWorld, session_id: Uuid) -> Result<Story, WorldError> {
        let now = Utc::now();
        self.npcs
            .seed_from_world_bible(session_id, &world.bible, now)?;
        for (id, facts) in &world.facts.characters {
            let (Ok(id), Some(location)) = (CharacterId::new(id.clone()), &facts.location) else {
                continue;
            };
            if self.npcs.get(session_id, &id).is_none() {
                continue;
            }
            let location = LocationId::new(location.clone())?;
            self.npcs
                .update(session_id, &id, |s| s.set_location(Some(location), now))?;
        }
        let narrative = NarrativeEngine::start(world.plan.clone(), world.facts.clone())?.state;
        // On stage at the start: whoever is where the player is.
        let here = narrative.world.player_location.as_deref();
        let active_npcs = self
            .npcs
            .list(session_id)
            .into_iter()
            .filter(|s| here.is_some() && s.location().map(LocationId::as_str) == here)
            .map(|s| s.character_id().as_str().to_owned())
            .collect();
        Ok(Story {
            narrative,
            active_npcs,
            replans: VecDeque::new(),
        })
    }

    /// Apply a validated player action: session first, then NPC perception
    /// and the narrative layer, then (for a meaningful action) the Director's
    /// context, built from the state the action left behind.
    ///
    /// All-or-nothing: a rejected action changes no session, NPC or narrative
    /// state, forms no memory and produces no follow-up.
    pub fn apply_player_action(&self, action: &ValidatedAction) -> Result<Applied, RuntimeError> {
        let session_id = action.session_id;
        let (Some(world), Some(story)) = (self.world.as_deref(), self.story(session_id)) else {
            let event = self.sessions.apply_action(action)?;
            return Ok(Applied {
                event,
                consequences: Vec::new(),
                replan: None,
                follow_up: None,
            });
        };
        let mut story = story.lock().expect("story lock poisoned");

        // 1. The authoritative session rules, on a draft. Duplicate and
        //    invalid actions stop here.
        let mut session = self.sessions.get(session_id).ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::SessionNotFound,
                format!("session {session_id} does not exist"),
            )
        })?;
        let now = Utc::now();
        let event = action::apply(&mut session, action, now)?;

        // 2. Who heard what. Computed now, committed in step 4.
        let roster = self.roster(session_id);
        let disclosed = adapt::disclosure(world, &roster, action);
        let npc_event = disclosed
            .as_ref()
            .map(|(secret, listener)| adapt::disclosure_event(&event, secret, listener, &roster))
            .transpose()?;

        // 3. Narrative consequences, on a draft.
        let mut narrative = story.narrative.clone();
        let mut changes = Changes::default();
        let reported = adapt::narrative_events(
            action,
            &event,
            disclosed
                .as_ref()
                .map(|(secret, listener)| (*secret, listener)),
            &narrative,
        );
        for narrative_event in &reported {
            let transition = NarrativeEngine::apply_event(&narrative, narrative_event)?;
            narrative = transition.state;
            changes.objectives.extend(transition.objective_changes);
            changes.missions.extend(transition.mission_changes);
            changes.checkpoints.extend(transition.checkpoint_changes);
            changes.effects.extend(transition.effects);
            changes.replan = transition.replan.or(changes.replan);
        }
        // Flags the plan set are world flags like any other.
        for effect in &changes.effects {
            if let Effect::SetFlag { flag, value } = effect {
                session.world_flags.insert(flag.clone(), *value);
            }
        }

        // 4. Commit. The NPC event is the last step that can fail, and it is
        //    atomic; after it nothing can.
        let memories = match &npc_event {
            Some(npc_event) => self.npcs.apply_event(npc_event)?,
            None => Vec::new(),
        };
        let emits = adapt::transition_emits(
            "narrative",
            &narrative,
            &changes.objectives,
            &changes.missions,
        );
        let consequences = adapt::record(&mut session, event.event_id, emits, now);
        story.narrative = narrative;
        self.sessions.replace(session.clone());

        // 5. The Director's view, from the state as it is now.
        let trigger = trigger_for(
            &story.narrative,
            disclosed
                .as_ref()
                .map(|(secret, listener)| (*secret, listener)),
            &changes,
        );
        let context = trigger.map(|trigger| {
            Box::new(view::build_context(
                world,
                &session,
                &story.narrative,
                &story.active_npcs,
                &self.npcs.list(session_id),
                trigger,
            ))
        });

        let follow_up = (context.is_some() || !memories.is_empty()).then_some(FollowUp {
            session_id,
            memories,
            context,
        });
        Ok(Applied {
            event,
            consequences,
            replan: changes.replan,
            follow_up,
        })
    }

    /// The slow half of an action: persist memories, then ask the Director at
    /// most once and apply what it decides. Never fails the action: provider
    /// and store failures are reported in the outcome and leave the state the
    /// action produced intact.
    pub async fn complete(&self, follow_up: FollowUp) -> FollowUpOutcome {
        let session_id = follow_up.session_id;
        let mut outcome = FollowUpOutcome {
            events: Vec::new(),
            director: DirectorReport::NotInvoked,
            memories_stored: 0,
            memory_errors: Vec::new(),
        };
        self.persist(&follow_up.memories, &mut outcome).await;

        let Some(context) = follow_up.context else {
            return outcome;
        };
        let decision = match self.director.decide(&context).await {
            Ok(decision) => decision,
            Err(err) => {
                warn!(%session_id, error = %err, "director produced no decision");
                outcome.director = DirectorReport::Failed(err);
                return outcome;
            }
        };
        match self.apply_decision(&decision) {
            Ok(applied) => {
                self.persist(&applied.memories, &mut outcome).await;
                info!(
                    %session_id,
                    decision_id = %decision.decision_id,
                    provider = %decision.metadata.provider,
                    actions = decision.actions.len(),
                    events = applied.events.len(),
                    "director decision applied"
                );
                outcome.events = applied.events;
                self.voice_dialogue(&mut outcome.events).await;
                outcome.director = DirectorReport::Applied {
                    decision: Box::new(decision),
                    deferred: applied.deferred,
                };
            }
            Err(error) => {
                warn!(%session_id, decision_id = %decision.decision_id, %error, "director decision rejected");
                outcome.director = DirectorReport::Rejected {
                    decision: Box::new(decision),
                    error,
                };
            }
        }
        outcome
    }

    /// [`Runtime::apply_player_action`] followed by [`Runtime::complete`].
    pub async fn process_player_action(
        &self,
        action: &ValidatedAction,
    ) -> Result<RuntimeOutcome, RuntimeError> {
        let applied = self.apply_player_action(action)?;
        let follow_up = match applied.follow_up {
            Some(follow_up) => Some(self.complete(follow_up).await),
            None => None,
        };
        Ok(RuntimeOutcome {
            event: applied.event,
            consequences: applied.consequences,
            replan: applied.replan,
            follow_up,
        })
    }

    /// Give each `dialogue_started` event an `audio_url` when the line can
    /// be voiced. Best-effort: on any failure the event goes out unchanged,
    /// as text. Only the outgoing copy is decorated; session state is not
    /// touched.
    async fn voice_dialogue(&self, events: &mut [WorldEvent]) {
        let Some(voice) = &self.voice else {
            return;
        };
        for event in events
            .iter_mut()
            .filter(|event| event.event_type == WorldEventType::DialogueStarted)
        {
            let field = |key: &str| event.payload.get(key).and_then(|v| v.as_str());
            let (Some(npc_id), Some(text)) = (field("npc_id"), field("text")) else {
                continue;
            };
            if let Some(url) = voice.voice_line(npc_id, text).await {
                event.payload["audio_url"] = url.into();
            }
        }
    }

    async fn persist(&self, memories: &[MemoryEntry], outcome: &mut FollowUpOutcome) {
        for memory in memories {
            match self.memory.store_memory(memory).await {
                Ok(()) => outcome.memories_stored += 1,
                Err(err) => {
                    warn!(memory_id = %memory.memory_id, error = %err, "could not persist npc memory");
                    outcome.memory_errors.push(err);
                }
            }
        }
    }

    /// Apply a Director decision to the session it was made for.
    ///
    /// The decision is revalidated against a context built from the state as
    /// it is at this moment, so a decision made stale by anything that
    /// happened since it was requested is rejected. All-or-nothing: every
    /// action is applied to drafts, and the drafts are committed only if all
    /// of them applied.
    pub fn apply_decision(
        &self,
        decision: &DirectorDecision,
    ) -> Result<AppliedDecision, ApplyError> {
        let session_id = decision.session_id;
        let (Some(world), Some(story)) = (self.world.as_deref(), self.story(session_id)) else {
            return Err(ApplyError::NoStory(session_id));
        };
        if decision.universe_id != world.universe_id {
            return Err(ApplyError::WrongUniverse {
                decision: decision.universe_id.clone(),
                session: world.universe_id.clone(),
            });
        }
        let mut story = story.lock().expect("story lock poisoned");
        let mut session = self
            .sessions
            .get(session_id)
            .ok_or(ApplyError::NoStory(session_id))?;
        let before = self.roster(session_id);

        let current = view::build_context(
            world,
            &session,
            &story.narrative,
            &story.active_npcs,
            &before.values().cloned().collect::<Vec<_>>(),
            decision.trigger.clone(),
        );
        validate_actions(&current, &decision.actions).map_err(ApplyError::Invalid)?;

        let now = Utc::now();
        let mut narrative = story.narrative.clone();
        let mut roster = before.clone();
        let mut active = story.active_npcs.clone();
        let output = apply::apply_actions(
            apply::Drafts {
                session: &mut session,
                narrative: &mut narrative,
                roster: &mut roster,
                active: &mut active,
            },
            decision,
            now,
        )?;

        // Commit. NPC drafts go in through the directory; under the story
        // lock nothing else writes this session, so this cannot fail halfway.
        for (id, state) in roster {
            if before.get(&id) != Some(&state) {
                self.npcs
                    .update(session_id, &id, |slot| {
                        *slot = state;
                        Ok(())
                    })
                    .map_err(ApplyError::Commit)?;
            }
        }
        let events = adapt::record(&mut session, decision.decision_id, output.emits, now);
        story.narrative = narrative;
        story.active_npcs = active;
        for note in &output.replans {
            if story.replans.len() == REPLAN_NOTES_CAP {
                story.replans.pop_front();
            }
            story.replans.push_back(note.clone());
        }
        self.sessions.replace(session);

        Ok(AppliedDecision {
            events,
            memories: output.memories,
            deferred: output.replans,
        })
    }
}

/// What applying a decision produced.
#[derive(Debug, Clone)]
pub struct AppliedDecision {
    /// World events for the client, already recorded in the session.
    pub events: Vec<WorldEvent>,
    /// Memories NPCs formed, still to be persisted.
    pub memories: Vec<MemoryEntry>,
    /// Replan requests that were recorded but not executed.
    pub deferred: Vec<ReplanNote>,
}

/// Narrative changes accumulated over one player action.
#[derive(Default)]
struct Changes {
    objectives: Vec<ObjectiveChange>,
    missions: Vec<MissionChange>,
    checkpoints: Vec<CheckpointChange>,
    effects: Vec<Effect>,
    replan: Option<ReplanRequest>,
}

/// Why the Director should look at this action, if it should at all. An
/// action that changed nothing in the story is not worth a decision.
fn trigger_for(
    narrative: &NarrativeState,
    disclosed: Option<(&Secret, &CharacterId)>,
    changes: &Changes,
) -> Option<Trigger> {
    // The Director is only shown objectives that have started.
    let visible = |objective_id: &String| {
        narrative
            .plan
            .objective(objective_id)
            .is_some_and(|(mission, objective)| {
                mission.status != MissionStatus::Inactive
                    && objective.status != ObjectiveStatus::Pending
            })
    };
    if let Some((secret, listener)) = disclosed {
        return Some(Trigger::PlayerDisclosure {
            npc_id: listener.as_str().to_owned(),
            objective_id: secret.objective_id.clone().filter(visible),
        });
    }

    let ended = |status: ObjectiveStatus| {
        changes
            .objectives
            .iter()
            .find(|c| c.to == status)
            .map(|c| c.objective_id.clone())
    };
    if let Some(objective_id) = ended(ObjectiveStatus::Failed) {
        return Some(Trigger::ObjectiveFailed { objective_id });
    }
    if let Some(objective_id) = ended(ObjectiveStatus::Completed) {
        return Some(Trigger::ObjectiveCompleted { objective_id });
    }
    if let Some(objective_id) = ended(ObjectiveStatus::Invalidated) {
        return Some(Trigger::ObjectiveFailed { objective_id });
    }
    let changed = !changes.objectives.is_empty()
        || !changes.missions.is_empty()
        || !changes.checkpoints.is_empty()
        || changes.replan.is_some();
    changed.then_some(Trigger::PlayerAction)
}
