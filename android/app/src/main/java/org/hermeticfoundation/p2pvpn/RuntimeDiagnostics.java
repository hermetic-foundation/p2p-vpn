package org.hermeticfoundation.p2pvpn;

final class RuntimeDiagnostics {
    final long peersWithoutSupportedPath;
    final long queueQueuedPackets;
    final long queueQueuedBytes;
    final long queueOldestPacketAgeMillis;
    final long queueDroppedPackets;
    final long queueDroppedBytes;
    final long queueExpiredPackets;
    final long queueExpiredBytes;
    final long outboundDroppedPackets;
    final long inboundDroppedPackets;
    final long packetPlaneInboundDroppedDatagrams;
    final long pathFallbacksToRelay;
    final long packetPlanePathDemotions;
    final long streamFallbackPathDemotions;
    final long blockedNoSupportedPathEvents;
    final long blockedPacketWindowEvents;

    private RuntimeDiagnostics(long[] values) {
        peersWithoutSupportedPath = values[0];
        queueQueuedPackets = values[1];
        queueQueuedBytes = values[2];
        queueOldestPacketAgeMillis = values[3];
        queueDroppedPackets = values[4];
        queueDroppedBytes = values[5];
        queueExpiredPackets = values[6];
        queueExpiredBytes = values[7];
        outboundDroppedPackets = values[8];
        inboundDroppedPackets = values[9];
        packetPlaneInboundDroppedDatagrams = values[10];
        pathFallbacksToRelay = values[11];
        packetPlanePathDemotions = values[12];
        streamFallbackPathDemotions = values[13];
        blockedNoSupportedPathEvents = values[14];
        blockedPacketWindowEvents = values[15];
    }

    static RuntimeDiagnostics empty() {
        return new RuntimeDiagnostics(new long[16]);
    }

    static RuntimeDiagnostics fromLines(Iterable<String> lines) {
        long[] values = new long[16];
        for (String line : lines) {
            int separator = line.indexOf(' ');
            if (separator <= 0 || separator == line.length() - 1) {
                continue;
            }
            long value;
            try {
                value = Long.parseLong(line.substring(separator + 1));
            } catch (NumberFormatException ignored) {
                continue;
            }
            if (value < 0) {
                continue;
            }
            int index = metricIndex(line.substring(0, separator));
            if (index >= 0) {
                values[index] = value;
            }
        }
        return new RuntimeDiagnostics(values);
    }

    private static int metricIndex(String name) {
        switch (name) {
            case "path_peers_without_supported_path":
                return 0;
            case "queue_queued_packets":
                return 1;
            case "queue_queued_bytes":
                return 2;
            case "queue_oldest_packet_age_millis":
                return 3;
            case "queue_dropped_packets":
                return 4;
            case "queue_dropped_bytes":
                return 5;
            case "queue_expired_packets":
                return 6;
            case "queue_expired_bytes":
                return 7;
            case "outbound_dropped_packets":
                return 8;
            case "inbound_dropped_packets":
                return 9;
            case "packet_plane_inbound_dropped_datagrams":
                return 10;
            case "path_fallbacks_to_relay":
                return 11;
            case "packet_plane_path_demotions":
                return 12;
            case "stream_fallback_path_demotions":
                return 13;
            case "outbound_queue_blocked_no_supported_path_events":
                return 14;
            case "outbound_queue_blocked_packet_window_events":
                return 15;
            default:
                return -1;
        }
    }
}
