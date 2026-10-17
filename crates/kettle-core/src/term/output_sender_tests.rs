use std::time::Duration;

use super::PtyOutputSender;

#[test]
fn best_effort_delivery_drops_when_its_bounded_queue_is_full() {
    let (tx, rx) = crossbeam_channel::bounded(1);
    let output = PtyOutputSender::best_effort(tx);
    output.send(vec![1]);
    output.send(vec![2]);

    assert_eq!(rx.recv().unwrap(), vec![1]);
    assert!(rx.try_recv().is_err());
}

#[test]
fn lossless_delivery_backpressures_until_the_receiver_drains() {
    let (tx, rx) = crossbeam_channel::bounded(1);
    let output = PtyOutputSender::lossless(tx);
    output.send(vec![1]);
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let sender = std::thread::spawn(move || {
        output.send(vec![2]);
        done_tx.send(()).unwrap();
    });

    assert!(done_rx.recv_timeout(Duration::from_millis(20)).is_err());
    assert_eq!(rx.recv().unwrap(), vec![1]);
    done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(rx.recv().unwrap(), vec![2]);
    sender.join().unwrap();
}
