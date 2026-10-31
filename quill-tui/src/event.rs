//! Merges terminal input, background task updates and the redraw timer.
use std::{sync::Arc, time::Duration};

use anyhow::Context as _;
use crossterm::event::EventStream;
use futures::StreamExt;
use quill_drivers::ConnectionPool;
use tokio::{
    sync::mpsc::UnboundedReceiver,
    time::{Interval, MissedTickBehavior},
};

use crate::connection::ConnectionName;

/// An update from a background task.
pub enum Msg {
    Connected {
        name: ConnectionName,
        result: quill_core::error::Result<Arc<dyn ConnectionPool>>,
    },
}

pub enum Event {
    Terminal(crossterm::event::Event),
    Msg(Msg),
    /// Time to redraw, if anything changed.
    Tick,
}

pub struct Events {
    terminal: EventStream,
    messages: UnboundedReceiver<Msg>,
    redraw: Interval,
}

impl Events {
    pub fn new(messages: UnboundedReceiver<Msg>, redraw: Duration) -> Self {
        let mut redraw = tokio::time::interval(redraw);
        redraw.set_missed_tick_behavior(MissedTickBehavior::Skip);
        Self {
            terminal: EventStream::new(),
            messages,
            redraw,
        }
    }

    pub async fn next(&mut self) -> anyhow::Result<Event> {
        tokio::select! {
            event = self.terminal.next() => {
                let event = event.context("terminal input closed")?;
                Ok(Event::Terminal(event.context("read terminal input")?))
            }
            // The app holds a sender, so the channel never closes while it runs.
            Some(msg) = self.messages.recv() => Ok(Event::Msg(msg)),
            _ = self.redraw.tick() => Ok(Event::Tick),
        }
    }
}
