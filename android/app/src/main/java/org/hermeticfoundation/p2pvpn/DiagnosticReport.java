package org.hermeticfoundation.p2pvpn;

import java.nio.charset.StandardCharsets;

final class DiagnosticReport {
    static final int SCHEMA_VERSION = 1;
    static final int MAX_BYTES = 64 * 1024;

    static final class Resources {
        final long processCpuMillis;
        final long totalPssKib;
        final long privateDirtyKib;
        final long javaHeapUsedBytes;
        final long javaHeapMaxBytes;
        final int activeThreads;

        Resources(
                long processCpuMillis,
                long totalPssKib,
                long privateDirtyKib,
                long javaHeapUsedBytes,
                long javaHeapMaxBytes,
                int activeThreads) {
            this.processCpuMillis = nonNegative(processCpuMillis);
            this.totalPssKib = nonNegative(totalPssKib);
            this.privateDirtyKib = nonNegative(privateDirtyKib);
            this.javaHeapUsedBytes = nonNegative(javaHeapUsedBytes);
            this.javaHeapMaxBytes = nonNegative(javaHeapMaxBytes);
            this.activeThreads = Math.max(0, activeThreads);
        }
    }

    static final class Input {
        final String generatedAt;
        final String appVersion;
        final int androidApi;
        final long serviceUptimeMillis;
        final boolean profileStored;
        final boolean profileReadable;
        final boolean connectionRequested;
        final boolean connected;
        final boolean alwaysOn;
        final boolean lockdown;
        final boolean busy;
        final long runtimeGeneration;
        final String underlayKind;
        final boolean underlayValidated;
        final int availableNetworks;
        final long selectionChanges;
        final long selectedLosses;
        final long recoveries;
        final long runtimeRecoveryRequests;
        final long runtimeRecoveryFailures;
        final boolean pairingActive;
        final boolean candidatePending;
        final RuntimeSummary paths;
        final RuntimeDiagnostics runtime;
        final Resources resources;
        final DiagnosticEventBuffer.Snapshot events;

        Input(
                String generatedAt,
                String appVersion,
                int androidApi,
                long serviceUptimeMillis,
                boolean profileStored,
                boolean profileReadable,
                boolean connectionRequested,
                boolean connected,
                boolean alwaysOn,
                boolean lockdown,
                boolean busy,
                long runtimeGeneration,
                String underlayKind,
                boolean underlayValidated,
                int availableNetworks,
                long selectionChanges,
                long selectedLosses,
                long recoveries,
                long runtimeRecoveryRequests,
                long runtimeRecoveryFailures,
                boolean pairingActive,
                boolean candidatePending,
                RuntimeSummary paths,
                RuntimeDiagnostics runtime,
                Resources resources,
                DiagnosticEventBuffer.Snapshot events) {
            this.generatedAt = bounded(generatedAt, 64, "unknown");
            this.appVersion = bounded(appVersion, 64, "unknown");
            this.androidApi = Math.max(0, androidApi);
            this.serviceUptimeMillis = nonNegative(serviceUptimeMillis);
            this.profileStored = profileStored;
            this.profileReadable = profileReadable;
            this.connectionRequested = connectionRequested;
            this.connected = connected;
            this.alwaysOn = alwaysOn;
            this.lockdown = lockdown;
            this.busy = busy;
            this.runtimeGeneration = nonNegative(runtimeGeneration);
            this.underlayKind = safeUnderlayKind(underlayKind);
            this.underlayValidated = underlayValidated;
            this.availableNetworks = Math.max(0, availableNetworks);
            this.selectionChanges = nonNegative(selectionChanges);
            this.selectedLosses = nonNegative(selectedLosses);
            this.recoveries = nonNegative(recoveries);
            this.runtimeRecoveryRequests = nonNegative(runtimeRecoveryRequests);
            this.runtimeRecoveryFailures = nonNegative(runtimeRecoveryFailures);
            this.pairingActive = pairingActive;
            this.candidatePending = candidatePending;
            this.paths = paths == null ? RuntimeSummary.empty() : paths;
            this.runtime = runtime == null ? RuntimeDiagnostics.empty() : runtime;
            this.resources = resources == null ? new Resources(0, 0, 0, 0, 0, 0) : resources;
            this.events =
                    events == null
                            ? new DiagnosticEventBuffer.Snapshot(
                                    java.util.Collections.emptyList(), 0)
                            : events;
        }
    }

    private DiagnosticReport() {}

    static String create(Input input) {
        StringBuilder report = new StringBuilder(8 * 1024);
        report.append('{');
        report.append("\"schema_version\":").append(SCHEMA_VERSION);
        report.append(",\"kind\":\"p2p-vpn-android-diagnostics\"");
        report.append(",\"generated_at\":").append(JsonStrings.quote(input.generatedAt));
        report.append(",\"app\":{");
        report.append("\"version\":").append(JsonStrings.quote(input.appVersion));
        report.append(",\"android_api\":").append(input.androidApi).append('}');
        appendLifecycle(report, input);
        appendUnderlay(report, input);
        appendPaths(report, input.paths, input.runtime);
        appendQueueAndDrops(report, input.runtime);
        appendResources(report, input.resources);
        report.append(",\"pairing\":{");
        report.append("\"operation_active\":").append(input.pairingActive);
        report.append(",\"candidate_pending\":").append(input.candidatePending).append('}');
        appendEvents(report, input.events);
        report.append(",\"privacy\":{");
        report.append("\"identity_material\":\"excluded\"");
        report.append(",\"peers\":\"excluded\"");
        report.append(",\"pairing_secrets\":\"excluded\"");
        report.append(",\"underlay_addresses\":\"excluded\"}");
        report.append('}');
        String encoded = report.toString();
        if (encoded.getBytes(StandardCharsets.UTF_8).length > MAX_BYTES) {
            throw new IllegalStateException("Diagnostic report exceeds its size limit");
        }
        return encoded;
    }

    private static void appendLifecycle(StringBuilder report, Input input) {
        report.append(",\"lifecycle\":{");
        report.append("\"service_uptime_millis\":").append(input.serviceUptimeMillis);
        report.append(",\"profile_stored\":").append(input.profileStored);
        report.append(",\"profile_readable\":").append(input.profileReadable);
        report.append(",\"connection_requested\":").append(input.connectionRequested);
        report.append(",\"connected\":").append(input.connected);
        report.append(",\"always_on\":").append(input.alwaysOn);
        report.append(",\"lockdown\":").append(input.lockdown);
        report.append(",\"busy\":").append(input.busy);
        report.append(",\"runtime_generation\":").append(input.runtimeGeneration).append('}');
    }

    private static void appendUnderlay(StringBuilder report, Input input) {
        report.append(",\"underlay\":{");
        report.append("\"kind\":").append(JsonStrings.quote(input.underlayKind));
        report.append(",\"validated\":").append(input.underlayValidated);
        report.append(",\"available_networks\":").append(input.availableNetworks);
        report.append(",\"selection_changes\":").append(input.selectionChanges);
        report.append(",\"selected_losses\":").append(input.selectedLosses);
        report.append(",\"recoveries\":").append(input.recoveries);
        report.append(",\"runtime_recovery_requests\":")
                .append(input.runtimeRecoveryRequests);
        report.append(",\"runtime_recovery_failures\":")
                .append(input.runtimeRecoveryFailures)
                .append('}');
    }

    private static void appendPaths(
            StringBuilder report, RuntimeSummary paths, RuntimeDiagnostics runtime) {
        report.append(",\"paths\":{");
        report.append("\"connected_peers\":").append(paths.connectedPeers);
        report.append(",\"peers_without_supported_path\":")
                .append(runtime.peersWithoutSupportedPath);
        report.append(",\"direct_udp_datagram\":").append(paths.directUdpDatagramPaths);
        report.append(",\"direct_quic_datagram\":").append(paths.directQuicDatagramPaths);
        report.append(",\"direct_quic_stream\":").append(paths.directQuicStreamPaths);
        report.append(",\"direct_tcp_stream\":").append(paths.directTcpStreamPaths);
        report.append(",\"relay\":").append(paths.relayPaths);
        report.append(",\"public_routing_peers\":").append(paths.publicRoutingPeers);
        report.append(",\"packet_plane_quic_sessions\":")
                .append(paths.packetPlaneQuicSessions);
        report.append(",\"promotions_to_direct\":")
                .append(paths.pathPromotionsToDirect)
                .append('}');
    }

    private static void appendQueueAndDrops(
            StringBuilder report, RuntimeDiagnostics runtime) {
        report.append(",\"queue\":{");
        report.append("\"queued_packets\":").append(runtime.queueQueuedPackets);
        report.append(",\"queued_bytes\":").append(runtime.queueQueuedBytes);
        report.append(",\"oldest_packet_age_millis\":")
                .append(runtime.queueOldestPacketAgeMillis);
        report.append(",\"blocked_no_supported_path_events\":")
                .append(runtime.blockedNoSupportedPathEvents);
        report.append(",\"blocked_packet_window_events\":")
                .append(runtime.blockedPacketWindowEvents)
                .append('}');
        report.append(",\"drops\":{");
        report.append("\"queue_packets\":").append(runtime.queueDroppedPackets);
        report.append(",\"queue_bytes\":").append(runtime.queueDroppedBytes);
        report.append(",\"expired_packets\":").append(runtime.queueExpiredPackets);
        report.append(",\"expired_bytes\":").append(runtime.queueExpiredBytes);
        report.append(",\"outbound_packets\":").append(runtime.outboundDroppedPackets);
        report.append(",\"inbound_packets\":").append(runtime.inboundDroppedPackets);
        report.append(",\"packet_plane_datagrams\":")
                .append(runtime.packetPlaneInboundDroppedDatagrams);
        report.append(",\"path_fallbacks_to_relay\":")
                .append(runtime.pathFallbacksToRelay);
        report.append(",\"packet_plane_path_demotions\":")
                .append(runtime.packetPlanePathDemotions);
        report.append(",\"stream_path_demotions\":")
                .append(runtime.streamFallbackPathDemotions)
                .append('}');
    }

    private static void appendResources(StringBuilder report, Resources resources) {
        report.append(",\"resources\":{");
        report.append("\"process_cpu_millis\":").append(resources.processCpuMillis);
        report.append(",\"total_pss_kib\":").append(resources.totalPssKib);
        report.append(",\"private_dirty_kib\":").append(resources.privateDirtyKib);
        report.append(",\"java_heap_used_bytes\":").append(resources.javaHeapUsedBytes);
        report.append(",\"java_heap_max_bytes\":").append(resources.javaHeapMaxBytes);
        report.append(",\"active_threads\":").append(resources.activeThreads).append('}');
    }

    private static void appendEvents(
            StringBuilder report, DiagnosticEventBuffer.Snapshot events) {
        report.append(",\"events\":{");
        report.append("\"discarded\":").append(events.discarded);
        report.append(",\"items\":[");
        boolean first = true;
        for (DiagnosticEventBuffer.Entry event : events.entries) {
            if (!first) {
                report.append(',');
            }
            first = false;
            report.append('{');
            report.append("\"sequence\":").append(event.sequence);
            report.append(",\"since_service_start_millis\":")
                    .append(event.sinceServiceStartMillis);
            report.append(",\"name\":").append(JsonStrings.quote(event.name));
            report.append('}');
        }
        report.append("]}");
    }

    private static long nonNegative(long value) {
        return Math.max(0, value);
    }

    private static String bounded(String value, int maximumLength, String fallback) {
        if (value == null || value.isEmpty() || value.length() > maximumLength) {
            return fallback;
        }
        return value;
    }

    private static String safeUnderlayKind(String value) {
        if ("ethernet".equals(value)
                || "wifi".equals(value)
                || "cellular".equals(value)
                || "bluetooth".equals(value)
                || "other".equals(value)
                || "none".equals(value)) {
            return value;
        }
        return "unknown";
    }
}
