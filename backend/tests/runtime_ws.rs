//! The Phase 2 runtime over a real WebSocket: the acknowledgement of a
//! `player_action` is still the first frame, and what the narrative layer and
//! the Director make of it follows as further `world_event`s.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use rift_backend::director::{DirectorEngine, ScriptedProvider};
use rift_backend::runtime::{Runtime, RuntimeWorld};
use rift_backend::session::SessionStore;
use rift_backend::{AppState, app};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const WORLD_BIBLE: &str = include_str!("fixtures/director/world_bible_breaking_bad.json");
const SCENARIO: &str = include_str!("fixtures/runtime/scenario_burner_phone.json");
const DECISION: &str = include_str!("fixtures/runtime/decision_after_disclosure.json");

async fn spawn_server() -> (String, AppState, Arc<ScriptedProvider>) {
    let world = RuntimeWorld::from_world_bible(WORLD_BIBLE)
        .and_then(|world| world.with_scenario_json(SCENARIO))
        .unwrap();
    let provider = Arc::new(ScriptedProvider::texts([DECISION]));
    let runtime = Runtime::new(SessionStore::new())
        .with_world(world)
        .with_director(DirectorEngine::new(provider.clone()));
    let state = AppState::new(runtime);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (format!("ws://{addr}/ws"), state, provider)
}

/// Send one client message; returns its `message_id`.
async fn send(ws: &mut Ws, message_type: &str, session_id: Option<Uuid>, payload: Value) -> String {
    let message_id = Uuid::new_v4().to_string();
    let text = json!({
        "protocol_version": 1,
        "message_id": message_id,
        "message_type": message_type,
        "timestamp": chrono::Utc::now(),
        "session_id": session_id,
        "payload": payload,
    })
    .to_string();
    ws.send(Message::text(text)).await.unwrap();
    message_id
}

async fn next(ws: &mut Ws) -> Value {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timed out waiting for a frame")
            .expect("connection closed")
            .unwrap();
        if let Message::Text(t) = msg {
            return serde_json::from_str(t.as_str()).unwrap();
        }
    }
}

fn player_action(sid: Uuid, action_type: &str, target: &str, content: Option<&str>) -> Value {
    let mut action = json!({
        "protocol_version": 1,
        "action_id": Uuid::new_v4(),
        "session_id": sid,
        "actor_id": "player",
        "action_type": action_type,
        "target": target,
        "timestamp": chrono::Utc::now(),
    });
    if let Some(content) = content {
        action["content"] = content.into();
    }
    action
}

#[tokio::test]
async fn divergence_reaches_the_client_as_world_events() {
    let (url, state, provider) = spawn_server().await;
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    send(&mut ws, "hello", None, json!({"client": "itest"})).await;
    assert_eq!(next(&mut ws).await["message_type"], "hello_ack");
    send(&mut ws, "create_session", None, json!({})).await;
    let created = next(&mut ws).await;
    assert_eq!(created["message_type"], "session_created");
    assert_eq!(
        created["payload"]["current_location"],
        "albuquerque_hospital"
    );
    let sid: Uuid = serde_json::from_value(created["session_id"].clone()).unwrap();

    // The protocol smoke action is unchanged even in a session with a world:
    // one acknowledgement, nothing else.
    let door = player_action(sid, "interact", "test_door", None);
    let door_id = send(&mut ws, "player_action", Some(sid), door.clone()).await;
    let ack = next(&mut ws).await;
    assert_eq!(ack["message_type"], "world_event");
    assert_eq!(ack["reply_to"], door_id);
    assert_eq!(ack["payload"]["event_type"], "interaction_acknowledged");
    assert_eq!(ack["payload"]["target"], "test_door");
    assert_eq!(ack["payload"]["sequence"], 1);
    send(&mut ws, "player_action", Some(sid), door).await;
    assert_eq!(next(&mut ws).await["payload"]["code"], "duplicate_action");
    assert_eq!(provider.call_count(), 0);

    // The divergence.
    let tell = player_action(
        sid,
        "speak",
        "hank_schrader",
        Some("Walter keeps a burner phone under the mattress."),
    );
    let tell_id = send(&mut ws, "player_action", Some(sid), tell).await;

    // First frame: the acknowledgement, exactly as before Phase 2.
    let ack = next(&mut ws).await;
    assert_eq!(ack["reply_to"], tell_id);
    assert_eq!(ack["payload"]["event_type"], "speech_acknowledged");
    assert_eq!(ack["payload"]["sequence"], 2);

    // Then everything that followed from it, pushed without a `reply_to`,
    // until the Director's last event.
    let mut pushed = Vec::new();
    loop {
        let frame = next(&mut ws).await;
        assert_eq!(frame["message_type"], "world_event");
        assert!(frame.get("reply_to").is_none(), "{frame}");
        assert_eq!(frame["protocol_version"], 1);
        assert_eq!(frame["session_id"], json!(sid));
        let payload = frame["payload"].clone();
        let last = payload["target"] == "choose_what_to_tell_hank";
        pushed.push(payload);
        if last {
            break;
        }
    }
    let sequences: Vec<u64> = pushed
        .iter()
        .map(|p| p["sequence"].as_u64().unwrap())
        .collect();
    let expected: Vec<u64> = (3..3 + pushed.len() as u64).collect();
    assert_eq!(sequences, expected, "pushed events arrive in session order");

    let of = |source: &str| -> Vec<String> {
        pushed
            .iter()
            .filter(|p| p["payload"]["source"] == source)
            .map(|p| p["event_type"].as_str().unwrap().to_owned())
            .collect()
    };
    // The narrative consequences come first, then the Director's answer.
    assert!(of("narrative").contains(&"mission_updated".to_owned()));
    assert!(of("narrative").contains(&"objective_updated".to_owned()));
    assert_eq!(
        of("director"),
        [
            "world_flag_changed",
            "npc_disposition_changed",
            "world_event_triggered",
            "dialogue_started",
            "objective_updated",
        ]
    );
    let failed = pushed
        .iter()
        .find(|p| p["target"] == "hide_burner_phone")
        .unwrap();
    assert_eq!(failed["payload"]["status"], "failed");
    assert_eq!(provider.call_count(), 1);

    // The connection is back with the player: a ping is answered next.
    send(&mut ws, "ping", None, json!({"nonce": "n"})).await;
    let pong = next(&mut ws).await;
    assert_eq!(pong["message_type"], "pong");

    let session = state.sessions.get(sid).unwrap();
    assert_eq!(session.event_count, 2 + pushed.len() as u64);
    assert_eq!(
        session.world_flags.get("hank_knows_about_phone"),
        Some(&true)
    );
}
