package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import java.nio.charset.StandardCharsets;
import java.util.Arrays;

import org.junit.Test;

public final class DiagnosticReportTest {
    @Test
    public void reportContainsAggregateHealthWithoutIdentityFields() {
        DiagnosticEventBuffer events = new DiagnosticEventBuffer();
        events.record("service_created", 0);
        events.record("underlay_recovery_completed", 1250);
        RuntimeSummary paths =
                RuntimeSummary.fromLines(
                        Arrays.asList(
                                "path_peers_with_supported_path 2",
                                "path_healthy_direct_quic_stream_paths 1",
                                "path_healthy_relay_paths 1",
                                "public_routing_peers 3"));
        RuntimeDiagnostics runtime =
                RuntimeDiagnostics.fromLines(
                        Arrays.asList(
                                "path_peers_without_supported_path 1",
                                "queue_queued_packets 4",
                                "queue_dropped_packets 5",
                                "outbound_dropped_packets 6",
                                "stream_fallback_path_demotions 7"));
        DiagnosticReport.Input input =
                new DiagnosticReport.Input(
                        "2026-08-30T12:00:00Z",
                        "0.1.0",
                        35,
                        2000,
                        true,
                        true,
                        true,
                        true,
                        true,
                        false,
                        false,
                        2,
                        "wifi-secret-handle",
                        true,
                        2,
                        4,
                        2,
                        1,
                        5,
                        0,
                        true,
                        false,
                        paths,
                        runtime,
                        new DiagnosticReport.Resources(100, 200, 300, 400, 500, 6),
                        events.snapshot());

        String report = DiagnosticReport.create(input);

        assertTrue(report.contains("\"kind\":\"p2p-vpn-android-diagnostics\""));
        assertTrue(report.contains("\"connected_peers\":2"));
        assertTrue(report.contains("\"always_on\":true"));
        assertTrue(report.contains("\"lockdown\":false"));
        assertTrue(report.contains("\"peers_without_supported_path\":1"));
        assertTrue(report.contains("\"queue_packets\":5"));
        assertTrue(report.contains("\"process_cpu_millis\":100"));
        assertTrue(report.contains("\"name\":\"underlay_recovery_completed\""));
        assertTrue(report.contains("\"kind\":\"unknown\""));
        assertFalse(report.contains("wifi-secret-handle"));
        assertFalse(report.contains("network_name"));
        assertFalse(report.contains("peer_id"));
        assertFalse(report.contains("pairing_code"));
        assertFalse(report.contains("hostname"));
        assertFalse(report.contains("vpn_ip"));
        assertTrue(report.getBytes(StandardCharsets.UTF_8).length <= DiagnosticReport.MAX_BYTES);
    }
}
