//! Input Stream — P1-P5 优先级队列。
//!
//! Spec §3.3: "Per-level tokio::sync::mpsc channels,
//! polled via tokio::select! with priority ordering."
//!
//! 优先级由事件循环的 `biased` select 强制执行，
//! 不是由数据结构本身。这里只负责创建和分发通道。

use tokio::sync::mpsc;
use crate::types::InputEvent;

pub struct InputSenders {
    pub p1: mpsc::Sender<InputEvent>,
    pub p2: mpsc::Sender<InputEvent>,
    pub p3: mpsc::Sender<InputEvent>,
    pub p4: mpsc::Sender<InputEvent>,
    pub p5: mpsc::Sender<InputEvent>,
}

pub struct InputReceivers {
    pub p1: mpsc::Receiver<InputEvent>,
    pub p2: mpsc::Receiver<InputEvent>,
    pub p3: mpsc::Receiver<InputEvent>,
    pub p4: mpsc::Receiver<InputEvent>,
    pub p5: mpsc::Receiver<InputEvent>,
}

pub fn create(buffer: usize) -> (InputSenders, InputReceivers) {
    let (p1_tx, p1_rx) = mpsc::channel(buffer);
    let (p2_tx, p2_rx) = mpsc::channel(buffer);
    let (p3_tx, p3_rx) = mpsc::channel(buffer);
    let (p4_tx, p4_rx) = mpsc::channel(buffer);
    let (p5_tx, p5_rx) = mpsc::channel(buffer);

    (
        InputSenders { p1: p1_tx, p2: p2_tx, p3: p3_tx, p4: p4_tx, p5: p5_tx },
        InputReceivers { p1: p1_rx, p2: p2_rx, p3: p3_rx, p4: p4_rx, p5: p5_rx },
    )
}