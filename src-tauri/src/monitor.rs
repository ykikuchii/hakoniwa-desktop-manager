use crate::types::{
    CommunicationEvent, CommunicationEventType, ConnectionDefinition, ConnectionSnapshot,
    ConnectionState, EventDirection, ObservationSource,
};
use chrono::{Duration, Utc};
use std::{collections::BTreeSet, sync::Mutex};
use uuid::Uuid;

const CONNECTED_WINDOW_SECONDS: i64 = 15;
const IDLE_WINDOW_SECONDS: i64 = 120;
const EVENT_LIMIT: usize = 4_000;

#[derive(Default)]
pub struct CommunicationMonitor {
    events: Mutex<Vec<CommunicationEvent>>,
    seen_log_entries: Mutex<BTreeSet<String>>,
}

impl CommunicationMonitor {
    pub fn record(&self, mut event: CommunicationEvent) {
        if event.id.is_empty() {
            event.id = Uuid::new_v4().to_string();
        }
        let mut events = self.events.lock().expect("monitor event lock poisoned");
        events.push(event);
        events.sort_by_key(|candidate| candidate.observed_at);
        if events.len() > EVENT_LIMIT {
            let remove = events.len() - EVENT_LIMIT;
            events.drain(0..remove);
        }
    }

    pub fn record_bridge_process_line(&self, connection_id: &str, process_id: &str, stream: &str, line_index: usize, line: &str) {
        let key = format!("{connection_id}:{process_id}:{stream}:{line_index}");
        let mut seen = self.seen_log_entries.lock().expect("monitor dedup lock poisoned");
        if !seen.insert(key) {
            return;
        }
        drop(seen);
        self.record_bridge_monitor_line(connection_id, line);
    }

    pub fn record_bridge_monitor_line(&self, connection_id: &str, line: &str) {
        let normalized = line.to_lowercase();
        let event_type = if normalized.contains("disconnect") || normalized.contains("closed") {
            CommunicationEventType::Disconnected
        } else if normalized.contains("error") || normalized.contains("failed") {
            CommunicationEventType::Error
        } else if normalized.contains("connect") || normalized.contains("healthy") {
            CommunicationEventType::Connected
        } else if normalized.contains("pdu") || normalized.contains("send") || normalized.contains("recv") || normalized.contains("message") {
            CommunicationEventType::Message
        } else {
            CommunicationEventType::Heartbeat
        };
        self.record(CommunicationEvent {
            id: String::new(),
            connection_id: connection_id.to_owned(),
            observed_at: Utc::now(),
            direction: infer_direction(&normalized),
            event_type,
            pdu_name: extract_pdu_name(line),
            byte_count: extract_byte_count(line),
            message: line.to_owned(),
            source: ObservationSource::BridgeMonitor,
        });
    }

    pub fn recent_events(&self, limit: usize) -> Vec<CommunicationEvent> {
        let events = self.events.lock().expect("monitor event lock poisoned");
        events.iter().rev().take(limit).cloned().collect()
    }

    pub fn snapshots(&self, definitions: &[ConnectionDefinition]) -> Vec<ConnectionSnapshot> {
        let events = self.events.lock().expect("monitor event lock poisoned");
        definitions.iter().map(|definition| snapshot_for(definition, &events)).collect()
    }
}

fn snapshot_for(definition: &ConnectionDefinition, events: &[CommunicationEvent]) -> ConnectionSnapshot {
    let relevant: Vec<&CommunicationEvent> = events.iter().filter(|event| event.connection_id == definition.id).collect();
    let last_activity = relevant.iter().filter(|event| matches!(event.event_type, CommunicationEventType::Message | CommunicationEventType::Heartbeat | CommunicationEventType::Connected)).map(|event| event.observed_at).max();
    let latest_error = relevant.iter().rev().find(|event| matches!(event.event_type, CommunicationEventType::Error | CommunicationEventType::Disconnected)).map(|event| event.message.clone());
    let mut sent = 0_u64;
    let mut received = 0_u64;
    let mut sent_bytes = 0_u64;
    let mut received_bytes = 0_u64;
    for event in &relevant {
        match event.direction {
            EventDirection::Sent => { sent += 1; sent_bytes += event.byte_count.unwrap_or_default(); }
            EventDirection::Received => { received += 1; received_bytes += event.byte_count.unwrap_or_default(); }
            EventDirection::Bidirectional => { sent += 1; received += 1; let bytes = event.byte_count.unwrap_or_default(); sent_bytes += bytes; received_bytes += bytes; }
            EventDirection::Lifecycle => {}
        }
    }
    let now = Utc::now();
    let state = match (last_activity, relevant.last()) {
        (Some(activity), _) if now - activity <= Duration::seconds(CONNECTED_WINDOW_SECONDS) => ConnectionState::Connected,
        (Some(activity), _) if now - activity <= Duration::seconds(IDLE_WINDOW_SECONDS) => ConnectionState::Idle,
        (Some(_), _) => ConnectionState::Disconnected,
        (None, Some(event)) if matches!(event.event_type, CommunicationEventType::Disconnected | CommunicationEventType::Error) => ConnectionState::Disconnected,
        _ => ConnectionState::Unknown,
    };
    let observation_source = relevant.last().map(|event| event.source.clone()).unwrap_or(ObservationSource::ConfigImport);
    ConnectionSnapshot {
        definition: definition.clone(),
        state,
        last_activity_at: last_activity,
        messages_sent: sent,
        messages_received: received,
        bytes_sent: sent_bytes,
        bytes_received: received_bytes,
        latest_error,
        observation_source,
    }
}

fn infer_direction(line: &str) -> EventDirection {
    match (line.contains("send"), line.contains("recv") || line.contains("receive")) {
        (true, true) => EventDirection::Bidirectional,
        (true, false) => EventDirection::Sent,
        (false, true) => EventDirection::Received,
        _ => EventDirection::Lifecycle,
    }
}

fn extract_pdu_name(line: &str) -> Option<String> {
    line.split_whitespace().find_map(|word| word.strip_prefix("pdu=").or_else(|| word.strip_prefix("pdu:")).map(|value| value.trim_matches(|character: char| !character.is_alphanumeric() && character != '_' && character != '/').to_owned()))
}

fn extract_byte_count(line: &str) -> Option<u64> {
    line.split_whitespace().find_map(|word| word.strip_prefix("bytes=").or_else(|| word.strip_prefix("size=")).and_then(|value| value.trim_matches(|character: char| !character.is_ascii_digit()).parse().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ConnectionDefinition, TransportKind};
    use std::collections::BTreeMap;

    fn connection() -> ConnectionDefinition {
        ConnectionDefinition { id: "c1".to_owned(), source: "a".to_owned(), destination: "b".to_owned(), label: "a to b".to_owned(), transport: TransportKind::Tcp, pdu_names: vec![], endpoint_config: None, details: BTreeMap::new(), source_asset_id: None, destination_asset_id: None, owner_asset_id: None }
    }

    #[test]
    fn records_message_as_connected() {
        let monitor = CommunicationMonitor::default();
        monitor.record_bridge_monitor_line("c1", "send pdu=pose bytes=32");
        assert_eq!(monitor.snapshots(&[connection()])[0].state, ConnectionState::Connected);
    }
}
