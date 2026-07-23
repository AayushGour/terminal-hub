use hub_relay::relay::RelayEvent;
use hub_relay::resize::spawn_coalescer;
use tokio::sync::mpsc;

#[tokio::test]
async fn coalesces_bursts_and_drops_unchanged() {
    let (out, mut rx) = mpsc::unbounded_channel::<RelayEvent>();
    let tx = spawn_coalescer(80, 24, out);

    // Burst of sizes within the window -> only the LAST should apply.
    tx.send((100, 30)).unwrap();
    tx.send((110, 33)).unwrap();
    tx.send((120, 40)).unwrap();

    let ev = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await.unwrap().unwrap();
    match ev { RelayEvent::ApplyResize(c, r) => { assert_eq!((c, r), (120, 40)); }, other => panic!("{other:?}") }

    // Re-send the SAME dims -> no emission (resize-only-if-changed).
    tx.send((120, 40)).unwrap();
    let none = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await;
    assert!(none.is_err(), "unchanged dims must not re-apply");
}
