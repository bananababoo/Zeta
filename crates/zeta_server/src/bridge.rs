//! Asynchronous bridge between Bevy ECS systems and the tokio runtime for AI DM calls.
//!
//! Rules: `docs/architecture/technical_architecture.md` Sections 1 and 2, and
//! `docs/architecture/ai_orchestration.md` Section 4.

use std::marker::PhantomData;
use std::sync::{Arc, Mutex, PoisonError};

use bevy_app::{App, Plugin, PreUpdate};
use bevy_ecs::prelude::{ResMut, Resource};
use thiserror::Error;
use tokio::runtime::{Builder, Handle, Runtime};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use zeta_core::ai::{AiProvider, AiRequest, ResolveError, ResolvedAction, resolve_action};
use zeta_core::catalog::Catalog;
use zeta_core::grid::MapBounds;
use zeta_core::pending_resolution::{PromptTicket, ResolutionLocks};
use zeta_core::snapshot::Snapshot;

/// Everything [`AiBridge`] needs to resolve 1 locked Freeform Action Prompt off the main loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolutionJob {
    /// Ticket identifying the active prompt in [`ResolutionLocks`].
    pub ticket: PromptTicket,
    /// Initial request sent to the AI provider.
    pub request: AiRequest,
    /// Snapshot the prompt locked; validation checks every referenced identifier against it.
    pub snapshot: Snapshot,
    /// Map bounds used when validating target grid squares.
    pub bounds: MapBounds,
    /// Maximum number of validation retries after the first response.
    pub retry_cap: u32,
}

/// Why an asynchronous AI resolution job finished without a valid [`ResolvedAction`].
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum BridgeFailure {
    /// The validation retry loop failed or the provider returned an error.
    #[error(transparent)]
    Resolution(#[from] ResolveError),
    /// The spawned `tokio` task panicked or was cancelled before delivering a result.
    #[error("the AI resolution task aborted before returning a result")]
    TaskAborted,
}

/// Result of 1 asynchronous AI resolution task, paired with its ticket and Snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletedResolution {
    /// Ticket of the prompt that finished, failed, or aborted.
    pub ticket: PromptTicket,
    /// Snapshot the prompt ran against, kept so callers can inspect or record it.
    pub snapshot: Snapshot,
    /// Validated action, or the failure that ended resolution without applying effects.
    pub outcome: Result<ResolvedAction, BridgeFailure>,
}

/// Holds the active and queued `PendingAiResolution` locks inside the Bevy world.
#[derive(Resource, Default, Debug)]
pub struct ResolutionLocksResource(pub ResolutionLocks);

/// Completed AI resolutions drained during [`PreUpdate`] for server systems to apply and release.
#[derive(Resource, Default, Debug)]
pub struct CompletedAiResolutions(pub Vec<CompletedResolution>);

/// Non-blocking bridge that runs AI resolution loops on `tokio` and drains results into Bevy.
#[derive(Debug, Resource)]
pub struct AiBridge<P> {
    // Dropped before the channel ends so cancelling tasks during shutdown can still send safely.
    _owned_runtime: Option<Runtime>,
    runtime: Handle,
    provider: Arc<P>,
    sender: UnboundedSender<CompletedResolution>,
    receiver: Mutex<UnboundedReceiver<CompletedResolution>>,
    in_flight: usize,
}

impl<P> AiBridge<P> {
    /// Creates a bridge that spawns jobs onto an existing `tokio` runtime `handle`.
    pub fn new(runtime: Handle, provider: Arc<P>) -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self {
            _owned_runtime: None,
            runtime,
            provider,
            sender,
            receiver: Mutex::new(receiver),
            in_flight: 0,
        }
    }

    /// Creates a bridge that owns a multi-thread `tokio` runtime for headless Bevy apps.
    ///
    /// # Errors
    ///
    /// Returns [`std::io::Error`] if the `tokio` runtime cannot be created.
    pub fn with_owned_runtime(provider: Arc<P>) -> Result<Self, std::io::Error> {
        let runtime = Builder::new_multi_thread().enable_all().build()?;
        let handle = runtime.handle().clone();
        let (sender, receiver) = mpsc::unbounded_channel();
        Ok(Self {
            _owned_runtime: Some(runtime),
            runtime: handle,
            provider,
            sender,
            receiver: Mutex::new(receiver),
            in_flight: 0,
        })
    }

    /// Drains every completed resolution currently buffered without blocking the caller.
    pub fn drain_completed(&mut self) -> Vec<CompletedResolution> {
        let mut completed = Vec::new();
        // A poisoned mutex only means a previous drainer panicked; the channel stays valid.
        let mut receiver = self.receiver.lock().unwrap_or_else(PoisonError::into_inner);
        while let Ok(item) = receiver.try_recv() {
            self.in_flight = self.in_flight.saturating_sub(1);
            completed.push(item);
        }
        completed
    }

    /// Number of spawned resolution jobs not yet drained by [`Self::drain_completed`].
    pub fn in_flight_count(&self) -> usize {
        self.in_flight
    }
}

impl<P: AiProvider + Send + Sync + 'static> AiBridge<P> {
    /// Spawns 1 AI resolution loop onto `tokio` and returns at once.
    ///
    /// Even if the task panics or is cancelled mid-flight, a [`BridgeFailure::TaskAborted`]
    /// result carrying the job's [`PromptTicket`] is sent so locks never leak.
    pub fn spawn_resolution(&mut self, job: ResolutionJob) {
        self.in_flight += 1;
        let provider = Arc::clone(&self.provider);
        let sender = self.sender.clone();
        self.runtime.spawn(async move {
            let mut guard = CompletionGuard(Some(PendingCompletion {
                ticket: job.ticket,
                snapshot: job.snapshot,
                sender,
            }));
            let outcome = if let Some(pending) = guard.0.as_ref() {
                resolve_action(
                    &*provider,
                    job.request,
                    &pending.snapshot,
                    job.bounds,
                    Catalog::srd(),
                    job.retry_cap,
                )
                .await
                .map_err(BridgeFailure::Resolution)
            } else {
                Err(BridgeFailure::TaskAborted)
            };
            guard.finish(outcome);
        });
    }
}

struct PendingCompletion {
    ticket: PromptTicket,
    snapshot: Snapshot,
    sender: UnboundedSender<CompletedResolution>,
}

/// Ensures every spawned job delivers its [`PromptTicket`] even on panic or task cancellation.
struct CompletionGuard(Option<PendingCompletion>);

impl CompletionGuard {
    fn finish(&mut self, outcome: Result<ResolvedAction, BridgeFailure>) {
        if let Some(pending) = self.0.take() {
            drop(pending.sender.send(CompletedResolution {
                ticket: pending.ticket,
                snapshot: pending.snapshot,
                outcome,
            }));
        }
    }
}

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        self.finish(Err(BridgeFailure::TaskAborted));
    }
}

/// Bevy [`PreUpdate`] system that moves finished `tokio` jobs into [`CompletedAiResolutions`].
pub fn drain_ai_resolutions<P: AiProvider + Send + Sync + 'static>(
    mut bridge: ResMut<AiBridge<P>>,
    mut completed: ResMut<CompletedAiResolutions>,
) {
    completed.0 = bridge.drain_completed();
}

/// Registers [`ResolutionLocksResource`], [`CompletedAiResolutions`], and
/// [`drain_ai_resolutions`] on [`PreUpdate`].
pub struct AiBridgePlugin<P>(PhantomData<fn() -> P>);

impl<P> AiBridgePlugin<P> {
    /// Creates the plugin.
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<P> Default for AiBridgePlugin<P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P> Clone for AiBridgePlugin<P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<P> Copy for AiBridgePlugin<P> {}

impl<P> std::fmt::Debug for AiBridgePlugin<P> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_tuple("AiBridgePlugin").finish()
    }
}

impl<P: AiProvider + Send + Sync + 'static> Plugin for AiBridgePlugin<P> {
    fn build(&self, app: &mut App) {
        app.init_resource::<ResolutionLocksResource>()
            .init_resource::<CompletedAiResolutions>()
            .add_systems(PreUpdate, drain_ai_resolutions::<P>);
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use bevy_app::Update;
    use tokio::sync::Notify;
    use zeta_core::ai::{AiExchange, AiMessage, AiResponse, ProviderError, ReplayProvider};
    use zeta_core::entity::EntityId;
    use zeta_core::grid::GridPos;
    use zeta_core::pending_resolution::Admission;
    use zeta_core::snapshot::{PerceptionSettings, SnapshotEntity};

    use super::*;

    const BOUNDS: MapBounds = MapBounds {
        width: 10,
        height: 10,
    };

    fn test_entity(world_id: u64, x: u32, y: u32) -> SnapshotEntity {
        SnapshotEntity {
            world_id: EntityId(world_id),
            name: format!("entity {world_id}"),
            position: GridPos::new(x, y),
            tags: vec![],
            passable: true,
            description: "A test entity.".into(),
            player_character: false,
        }
    }

    fn snapshot_of(entities: &[SnapshotEntity]) -> Snapshot {
        let (actor, rest) = entities.split_first().expect("at least the actor");
        Snapshot::build(
            actor.clone(),
            rest.iter().cloned(),
            |_| false,
            PerceptionSettings {
                hearing_range_squares: 100,
                max_count: 100,
            },
        )
    }

    fn test_request(prompt: &str) -> AiRequest {
        AiRequest {
            system_prompt: "system".into(),
            messages: vec![AiMessage::User {
                text: prompt.into(),
            }],
            response_schema: None,
        }
    }

    fn valid_response(narration: &str) -> AiResponse {
        AiResponse {
            text: format!(
                r#"{{"ok": true, "chk": null, "fx": [{{"op": "narrate", "text": "{narration}"}}], "fail": []}}"#
            ),
            provider_turn: None,
        }
    }

    struct GatedProvider {
        started: Arc<Notify>,
        gate: Arc<Notify>,
        narration: &'static str,
    }

    impl AiProvider for GatedProvider {
        async fn complete(&self, _request: &AiRequest) -> Result<AiResponse, ProviderError> {
            self.started.notify_one();
            self.gate.notified().await;
            Ok(valid_response(self.narration))
        }
    }

    struct PanickingProvider {
        started: Arc<Notify>,
    }

    impl AiProvider for PanickingProvider {
        async fn complete(&self, _request: &AiRequest) -> Result<AiResponse, ProviderError> {
            self.started.notify_one();
            panic!("simulated provider panic");
        }
    }

    struct NeverEndingProvider {
        started: Arc<Notify>,
    }

    impl AiProvider for NeverEndingProvider {
        async fn complete(&self, _request: &AiRequest) -> Result<AiResponse, ProviderError> {
            self.started.notify_one();
            std::future::pending::<()>().await;
            unreachable!("pending never resolves")
        }
    }

    #[test]
    fn bevy_update_never_blocks_while_tokio_waits_for_provider() {
        let started = Arc::new(Notify::new());
        let gate = Arc::new(Notify::new());
        let provider = Arc::new(GatedProvider {
            started: Arc::clone(&started),
            gate: Arc::clone(&gate),
            narration: "Unblocked.",
        });

        let runtime = Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("tokio runtime");
        let bridge = AiBridge::new(runtime.handle().clone(), provider);

        let mut app = App::new();
        app.add_plugins(AiBridgePlugin::<GatedProvider>::new())
            .insert_resource(bridge);

        let snapshot = snapshot_of(&[test_entity(1, 0, 0)]);
        let (ticket, admission) = app
            .world_mut()
            .resource_mut::<ResolutionLocksResource>()
            .0
            .submit(&snapshot);
        assert_eq!(admission, Admission::Locked);

        app.world_mut()
            .resource_mut::<AiBridge<GatedProvider>>()
            .spawn_resolution(ResolutionJob {
                ticket,
                request: test_request("I wait."),
                snapshot: snapshot.clone(),
                bounds: BOUNDS,
                retry_cap: 0,
            });

        runtime
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(2), started.notified()).await
            })
            .expect("provider started");

        let tick_start = Instant::now();
        app.update();
        assert!(
            tick_start.elapsed() < Duration::from_millis(200),
            "app.update() blocked for {:?}",
            tick_start.elapsed()
        );
        assert_eq!(
            app.world()
                .resource::<AiBridge<GatedProvider>>()
                .in_flight_count(),
            1
        );
        assert_eq!(
            app.world().resource::<CompletedAiResolutions>().0,
            Vec::new()
        );

        gate.notify_one();

        for _ in 0..400 {
            app.update();
            if app
                .world()
                .resource::<AiBridge<GatedProvider>>()
                .in_flight_count()
                == 0
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        assert_eq!(
            app.world()
                .resource::<AiBridge<GatedProvider>>()
                .in_flight_count(),
            0
        );
        let completed = &app.world().resource::<CompletedAiResolutions>().0;
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].ticket, ticket);
        assert_eq!(completed[0].snapshot, snapshot);
        assert!(completed[0].outcome.is_ok());
    }

    #[derive(Resource, Default)]
    struct QueuedPrompts(Vec<(PromptTicket, AiRequest, Snapshot)>);

    #[derive(Resource, Default)]
    struct ResolvedLog(Vec<CompletedResolution>);

    fn process_completed_resolutions(
        mut completed: ResMut<CompletedAiResolutions>,
        mut locks: ResMut<ResolutionLocksResource>,
        mut bridge: ResMut<AiBridge<ReplayProvider>>,
        mut queued: ResMut<QueuedPrompts>,
        mut log: ResMut<ResolvedLog>,
    ) {
        for item in std::mem::take(&mut completed.0) {
            let ticket = item.ticket;
            log.0.push(item);
            let ready = locks.0.release(ticket).expect("active ticket releases");
            for ready_ticket in ready {
                let position = queued
                    .0
                    .iter()
                    .position(|(queued_ticket, _, _)| *queued_ticket == ready_ticket)
                    .expect("queued prompt exists for ready ticket");
                let (_, request, fresh_snapshot) = queued.0.remove(position);
                let admission = locks
                    .0
                    .relock(ready_ticket, &fresh_snapshot)
                    .expect("ready ticket relocks");
                assert_eq!(admission, Admission::Locked);
                bridge.spawn_resolution(ResolutionJob {
                    ticket: ready_ticket,
                    request,
                    snapshot: fresh_snapshot,
                    bounds: BOUNDS,
                    retry_cap: 0,
                });
            }
        }
    }

    #[test]
    fn completed_resolution_releases_locks_and_dispatches_queued_prompt() {
        let first_request = test_request("First prompt");
        let second_request = test_request("Second prompt");
        let replay = Arc::new(ReplayProvider::new(vec![
            AiExchange {
                request: first_request.clone(),
                response: valid_response("First resolved."),
            },
            AiExchange {
                request: second_request.clone(),
                response: valid_response("Second resolved."),
            },
        ]));

        let bridge =
            AiBridge::with_owned_runtime(Arc::clone(&replay)).expect("bridge with owned runtime");

        let mut app = App::new();
        app.add_plugins(AiBridgePlugin::<ReplayProvider>::new())
            .insert_resource(bridge)
            .init_resource::<QueuedPrompts>()
            .init_resource::<ResolvedLog>()
            .add_systems(Update, process_completed_resolutions);

        let first_snapshot = snapshot_of(&[test_entity(1, 0, 0), test_entity(2, 1, 0)]);
        let overlapping_snapshot = snapshot_of(&[test_entity(3, 0, 0), test_entity(2, 1, 0)]);
        let fresh_second_snapshot = snapshot_of(&[test_entity(3, 2, 0), test_entity(2, 1, 0)]);

        let (first_ticket, first_admission) = app
            .world_mut()
            .resource_mut::<ResolutionLocksResource>()
            .0
            .submit(&first_snapshot);
        assert_eq!(first_admission, Admission::Locked);

        let (second_ticket, second_admission) = app
            .world_mut()
            .resource_mut::<ResolutionLocksResource>()
            .0
            .submit(&overlapping_snapshot);
        assert_eq!(second_admission, Admission::Queued);

        app.world_mut().resource_mut::<QueuedPrompts>().0.push((
            second_ticket,
            second_request,
            fresh_second_snapshot.clone(),
        ));

        app.world_mut()
            .resource_mut::<AiBridge<ReplayProvider>>()
            .spawn_resolution(ResolutionJob {
                ticket: first_ticket,
                request: first_request,
                snapshot: first_snapshot,
                bounds: BOUNDS,
                retry_cap: 0,
            });

        for _ in 0..400 {
            app.update();
            if app.world().resource::<ResolvedLog>().0.len() == 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        let log = &app.world().resource::<ResolvedLog>().0;
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].ticket, first_ticket);
        assert!(log[0].outcome.is_ok());
        assert_eq!(log[1].ticket, second_ticket);
        assert_eq!(log[1].snapshot, fresh_second_snapshot);
        assert!(log[1].outcome.is_ok());

        let locks = &app.world().resource::<ResolutionLocksResource>().0;
        assert!(!locks.is_locked(EntityId(1)));
        assert!(!locks.is_locked(EntityId(2)));
        assert!(!locks.is_locked(EntityId(3)));
        assert_eq!(locks.queued_count(), 0);
        assert_eq!(
            app.world()
                .resource::<AiBridge<ReplayProvider>>()
                .in_flight_count(),
            0
        );
        assert!(replay.is_finished());
    }

    #[test]
    fn aborted_or_panicking_task_still_delivers_ticket_so_locks_can_release() {
        // Case 1: task panics inside the provider.
        let panic_started = Arc::new(Notify::new());
        let panic_provider = Arc::new(PanickingProvider {
            started: Arc::clone(&panic_started),
        });
        let mut panic_bridge =
            AiBridge::with_owned_runtime(panic_provider).expect("bridge with owned runtime");
        let mut locks = ResolutionLocks::new();
        let snapshot = snapshot_of(&[test_entity(1, 0, 0), test_entity(2, 1, 0)]);
        let (panic_ticket, admission) = locks.submit(&snapshot);
        assert_eq!(admission, Admission::Locked);
        assert!(locks.is_locked(EntityId(1)));

        panic_bridge.spawn_resolution(ResolutionJob {
            ticket: panic_ticket,
            request: test_request("Panic"),
            snapshot: snapshot.clone(),
            bounds: BOUNDS,
            retry_cap: 0,
        });

        let mut drained = Vec::new();
        for _ in 0..400 {
            drained.extend(panic_bridge.drain_completed());
            if !drained.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].ticket, panic_ticket);
        assert_eq!(drained[0].snapshot, snapshot);
        assert_eq!(drained[0].outcome, Err(BridgeFailure::TaskAborted));
        assert_eq!(panic_bridge.in_flight_count(), 0);
        locks
            .release(drained[0].ticket)
            .expect("aborted ticket releases lock");
        assert!(!locks.is_locked(EntityId(1)));

        // Case 2: pending task is cancelled when its runtime shuts down.
        let abort_started = Arc::new(Notify::new());
        let abort_provider = Arc::new(NeverEndingProvider {
            started: Arc::clone(&abort_started),
        });
        let runtime = Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("tokio runtime");
        let mut abort_bridge = AiBridge::new(runtime.handle().clone(), abort_provider);
        let (abort_ticket, admission) = locks.submit(&snapshot);
        assert_eq!(admission, Admission::Locked);

        abort_bridge.spawn_resolution(ResolutionJob {
            ticket: abort_ticket,
            request: test_request("Cancel"),
            snapshot: snapshot.clone(),
            bounds: BOUNDS,
            retry_cap: 0,
        });
        runtime
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(2), abort_started.notified()).await
            })
            .expect("abort task started");
        runtime.shutdown_timeout(Duration::from_millis(10));

        let mut abort_drained = Vec::new();
        for _ in 0..400 {
            abort_drained.extend(abort_bridge.drain_completed());
            if !abort_drained.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(abort_drained.len(), 1);
        assert_eq!(abort_drained[0].ticket, abort_ticket);
        assert_eq!(abort_drained[0].outcome, Err(BridgeFailure::TaskAborted));
        assert_eq!(abort_bridge.in_flight_count(), 0);
        locks
            .release(abort_drained[0].ticket)
            .expect("cancelled ticket releases lock");
        assert!(!locks.is_locked(EntityId(1)));
    }
}
