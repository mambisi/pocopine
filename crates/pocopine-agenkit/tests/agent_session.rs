#![cfg(not(target_arch = "wasm32"))]
//! The durable session stores (JSONL + SQLite) survive a process restart and
//! persist forks; the SQLite-backed thread store keeps its owner scope too.

use std::sync::Arc;

use pocopine_agenkit::server::session::{
    JsonlSessionStore, Session, SessionStore, SqliteSessionStore, ThreadId,
};
use pocopine_agenkit::server::{AgentThreadStore, AuthUser, Principal, SessionThreadStore};
use pocopine_agenkit_core::{Role, ThreadMessage, ThreadRetention};

#[tokio::test]
async fn jsonl_store_survives_a_reopen() {
    let dir = tempfile::tempdir().unwrap();

    // First "process": create a thread, append two messages, checkpoint, append.
    let id: ThreadId = {
        let store: Arc<dyn SessionStore> = Arc::new(JsonlSessionStore::new(dir.path()));
        let s = Session::create(store, None).await.unwrap();
        s.append_message(serde_json::json!({ "role": "user", "text": "one" }))
            .await
            .unwrap();
        s.append_message(serde_json::json!({ "role": "assistant", "text": "two" }))
            .await
            .unwrap();
        s.checkpoint(serde_json::json!({ "summary": "greeting" }))
            .await
            .unwrap();
        s.append_message(serde_json::json!({ "role": "user", "text": "three" }))
            .await
            .unwrap();
        s.id().clone()
    }; // store dropped — simulates process exit

    // Second "process": a fresh store over the same dir resumes the thread.
    let store: Arc<dyn SessionStore> = Arc::new(JsonlSessionStore::new(dir.path()));
    let resumed = Session::open(store.clone(), id.clone());

    // Full history round-trips: 2 messages + checkpoint + 1 message = 4.
    let history = resumed.history().await.unwrap();
    assert_eq!(history.len(), 4);
    assert_eq!(
        history[0].data,
        serde_json::json!({ "role": "user", "text": "one" })
    );
    assert_eq!(
        history[3].data,
        serde_json::json!({ "role": "user", "text": "three" })
    );

    // Active context resumes from the checkpoint: [checkpoint, "three"].
    let active = resumed.active_context().await.unwrap();
    assert_eq!(active.len(), 2);

    // last_seq reflects the persisted record count (4), so the next append is seq 4.
    let meta = store.meta(&id).await.unwrap().unwrap();
    assert_eq!(meta.last_seq, 4);
    let next = resumed
        .append_message(serde_json::json!({ "role": "assistant", "text": "four" }))
        .await
        .unwrap();
    assert_eq!(next.seq, 4);
}

#[tokio::test]
async fn jsonl_store_persists_forks_and_their_tree() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn SessionStore> = Arc::new(JsonlSessionStore::new(dir.path()));

    let parent = Session::create(store.clone(), None).await.unwrap();
    parent
        .append_message(serde_json::json!({ "m": 0 }))
        .await
        .unwrap();
    parent
        .append_message(serde_json::json!({ "m": 1 }))
        .await
        .unwrap();
    let child = parent.fork(1).await.unwrap();
    child
        .append_message(serde_json::json!({ "c": 0 }))
        .await
        .unwrap();

    // Reopen and verify the branch survived: child = inherited m0 + its c0.
    let store2: Arc<dyn SessionStore> = Arc::new(JsonlSessionStore::new(dir.path()));
    let child2 = Session::open(store2.clone(), child.id().clone());
    let ch = child2.history().await.unwrap();
    assert_eq!(ch.len(), 2);
    assert_eq!(ch[0].data, serde_json::json!({ "m": 0 }));
    assert_eq!(ch[1].data, serde_json::json!({ "c": 0 }));

    // The parent still lists the child (the tree persisted).
    let kids = store2.children(parent.id()).await.unwrap();
    assert_eq!(kids, vec![child.id().clone()]);

    // The parent itself is untouched (still 2 records).
    assert_eq!(
        Session::open(store2, parent.id().clone())
            .history()
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn sqlite_store_survives_a_reopen_and_persists_a_fork() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("threads.db");

    let (id, branch_id) = {
        let store: Arc<dyn SessionStore> = Arc::new(SqliteSessionStore::open(&db).unwrap());
        let chat = Session::create(store.clone(), None).await.unwrap();
        chat.append_message(serde_json::json!({ "text": "one" }))
            .await
            .unwrap();
        chat.append_message(serde_json::json!({ "text": "two" }))
            .await
            .unwrap();
        let branch = chat.fork(1).await.unwrap();
        branch
            .append_message(serde_json::json!({ "text": "branch" }))
            .await
            .unwrap();
        (chat.id().clone(), branch.id().clone())
    }; // store dropped — simulates process exit

    // Reopen over the same file.
    let store: Arc<dyn SessionStore> = Arc::new(SqliteSessionStore::open(&db).unwrap());
    // The root history survived (2 messages).
    assert_eq!(
        Session::open(store.clone(), id.clone())
            .history()
            .await
            .unwrap()
            .len(),
        2
    );
    // The branch survived: inherited [one] + its own [branch].
    let bh = Session::open(store.clone(), branch_id)
        .history()
        .await
        .unwrap();
    assert_eq!(bh.len(), 2);
    assert_eq!(bh[1].data, serde_json::json!({ "text": "branch" }));
    // children() (the indexed parent lookup) still lists the fork.
    assert_eq!(store.children(&id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn sqlite_thread_store_keeps_owner_scope_across_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("threads.db");
    let alice_p = Principal::from_user(AuthUser::new("alice"));
    let bob_p = Principal::from_user(AuthUser::new("bob"));
    let alice = pocopine_agenkit::server::ThreadOwner::from_principal(&alice_p);
    let bob = pocopine_agenkit::server::ThreadOwner::from_principal(&bob_p);

    // First "process": Alice creates a SQLite-backed thread and appends.
    let id = {
        let store = SessionThreadStore::new(Arc::new(SqliteSessionStore::open(&db).unwrap()));
        let id = store
            .create("agent", alice, ThreadRetention::Durable)
            .await
            .unwrap();
        store
            .append(
                &id,
                alice,
                vec![ThreadMessage::new(Role::User, "remembered")],
            )
            .await
            .unwrap();
        id
    };

    // Second "process": a fresh store over the same db.
    let store = SessionThreadStore::new(Arc::new(SqliteSessionStore::open(&db).unwrap()));
    let history = store.load(&id, alice).await.unwrap().unwrap();
    assert_eq!(history.len(), 1);
    assert!(history[0].content.as_text().contains("remembered"));
    // Owner scope survived the restart — Bob still can't read it.
    assert!(store.load(&id, bob).await.unwrap().is_none());
}
