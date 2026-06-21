use super::*;
use crate::{SyncDeviceId, SyncLocalIdentity, SyncLocalStore};

#[tokio::test]
async fn memory_store_persists_identity() {
    let store = MemoryLocalStore::new();
    let identity =
        SyncLocalIdentity::with_next_counter(SyncDeviceId::new("device_abc").unwrap(), 7).unwrap();

    assert!(store.load_identity().await.unwrap().is_none());
    store.save_identity(identity.clone()).await.unwrap();

    assert_eq!(store.load_identity().await.unwrap(), Some(identity));
}

#[tokio::test]
async fn memory_store_reserves_mutation_ids_and_persists_counter() {
    let store = MemoryLocalStore::new();
    let identity =
        SyncLocalIdentity::with_next_counter(SyncDeviceId::new("device_abc").unwrap(), 7).unwrap();
    store.save_identity(identity).await.unwrap();

    let first = store.reserve_mutation_id().await.unwrap();
    let second = store.reserve_mutation_id().await.unwrap();

    assert_eq!(first.as_str(), "device_abc:7");
    assert_eq!(second.as_str(), "device_abc:8");
    assert_eq!(
        store
            .load_identity()
            .await
            .unwrap()
            .unwrap()
            .next_mutation_counter,
        9
    );
}

#[tokio::test]
async fn memory_store_reserve_creates_identity_when_missing() {
    let store = MemoryLocalStore::new();

    let id = store.reserve_mutation_id().await.unwrap();
    let identity = store.load_identity().await.unwrap().unwrap();

    assert!(id.as_str().starts_with(identity.device_id.as_str()));
    assert!(id.as_str().ends_with(":1"));
    assert_eq!(identity.next_mutation_counter, 2);
}

#[tokio::test]
async fn memory_store_reserve_overflow_does_not_advance_counter() {
    let store = MemoryLocalStore::new();
    let identity =
        SyncLocalIdentity::with_next_counter(SyncDeviceId::new("device_abc").unwrap(), u64::MAX)
            .unwrap();
    store.save_identity(identity.clone()).await.unwrap();

    let err = store.reserve_mutation_id().await.unwrap_err();

    assert!(err.to_string().contains("next mutation counter"));
    assert_eq!(store.load_identity().await.unwrap(), Some(identity));
}
