package org.hermeticfoundation.p2pvpn;

import java.util.Locale;

final class RuntimeSummary {
    final long connectedPeers;
    final long directPaths;
    final long directUdpDatagramPaths;
    final long directQuicDatagramPaths;
    final long directQuicStreamPaths;
    final long directTcpStreamPaths;
    final long relayPaths;
    final long publicRoutingPeers;

    private RuntimeSummary(
            long connectedPeers,
            long directUdpDatagramPaths,
            long directQuicDatagramPaths,
            long directQuicStreamPaths,
            long directTcpStreamPaths,
            long relayPaths,
            long publicRoutingPeers) {
        this.connectedPeers = connectedPeers;
        this.directUdpDatagramPaths = directUdpDatagramPaths;
        this.directQuicDatagramPaths = directQuicDatagramPaths;
        this.directQuicStreamPaths = directQuicStreamPaths;
        this.directTcpStreamPaths = directTcpStreamPaths;
        this.directPaths =
                saturatingAdd(
                        saturatingAdd(directUdpDatagramPaths, directQuicDatagramPaths),
                        saturatingAdd(directQuicStreamPaths, directTcpStreamPaths));
        this.relayPaths = relayPaths;
        this.publicRoutingPeers = publicRoutingPeers;
    }

    static RuntimeSummary empty() {
        return new RuntimeSummary(0, 0, 0, 0, 0, 0, 0);
    }

    static RuntimeSummary fromLines(Iterable<String> lines) {
        long connectedPeers = 0;
        long directUdpDatagramPaths = 0;
        long directQuicDatagramPaths = 0;
        long directQuicStreamPaths = 0;
        long directTcpStreamPaths = 0;
        long relayPaths = 0;
        long publicRoutingPeers = 0;
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
            switch (line.substring(0, separator)) {
                case "path_peers_with_supported_path":
                    connectedPeers = value;
                    break;
                case "path_healthy_direct_udp_datagram_paths":
                    directUdpDatagramPaths = saturatingAdd(directUdpDatagramPaths, value);
                    break;
                case "path_healthy_direct_quic_datagram_paths":
                    directQuicDatagramPaths = saturatingAdd(directQuicDatagramPaths, value);
                    break;
                case "path_healthy_direct_quic_stream_paths":
                    directQuicStreamPaths = saturatingAdd(directQuicStreamPaths, value);
                    break;
                case "path_healthy_direct_tcp_stream_paths":
                    directTcpStreamPaths = saturatingAdd(directTcpStreamPaths, value);
                    break;
                case "path_healthy_relay_paths":
                    relayPaths = value;
                    break;
                case "public_routing_peers":
                    publicRoutingPeers = value;
                    break;
                default:
                    break;
            }
        }
        return new RuntimeSummary(
                connectedPeers,
                directUdpDatagramPaths,
                directQuicDatagramPaths,
                directQuicStreamPaths,
                directTcpStreamPaths,
                relayPaths,
                publicRoutingPeers);
    }

    String describe() {
        return String.format(
                Locale.ROOT,
                "Overlay peers: %d connected; paths: %d direct, %d relay; public routers: %d",
                connectedPeers,
                directPaths,
                relayPaths,
                publicRoutingPeers);
    }

    private static long saturatingAdd(long left, long right) {
        if (Long.MAX_VALUE - left < right) {
            return Long.MAX_VALUE;
        }
        return left + right;
    }
}
