package org.hermeticfoundation.p2pvpn;

import android.app.Activity;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.net.VpnService;
import android.os.Build;
import android.util.Base64;

import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

import java.nio.charset.StandardCharsets;

public final class DebugAutomationReceiver extends BroadcastReceiver {
    static final String ACTION = "org.hermeticfoundation.p2pvpn.debug.AUTOMATION";
    private static final int SCHEMA_VERSION = 1;

    @Override
    public void onReceive(Context context, Intent intent) {
        if (intent == null || !ACTION.equals(intent.getAction())) {
            respond(false, null, "invalid_action");
            return;
        }
        try {
            String command = requiredExtra(intent, "command", 32);
            switch (command) {
                case "status":
                    status(context);
                    return;
                case "diagnostics":
                    diagnostics(context);
                    return;
                case "create-profile":
                    createProfile(context, intent);
                    return;
                case "join-pairing":
                    enqueue(context, command, requiredExtra(intent, "code", 64));
                    return;
                case "approve-pairing":
                    enqueue(context, command, optionalExtra(intent, "hostname", 63));
                    return;
                case "open-pairing":
                case "reject-pairing":
                    enqueue(context, command, null);
                    return;
                case "connect":
                    connect(context);
                    return;
                case "disconnect":
                    startService(context, P2pVpnService.ACTION_DISCONNECT);
                    accepted(command);
                    return;
                default:
                    respond(false, null, "unsupported_command");
            }
        } catch (IllegalArgumentException error) {
            respond(false, null, error.getMessage());
        } catch (RuntimeException | JSONException error) {
            respond(false, null, "automation_internal_error");
        }
    }

    private void status(Context context) throws JSONException {
        P2pVpnService.Snapshot snapshot = P2pVpnService.debugSnapshot();
        JSONObject value = new JSONObject();
        value.put("service_ready", snapshot != null);
        if (snapshot == null) {
            enqueueService(context, "ensure", null);
        } else {
            value.put("snapshot", snapshotJson(snapshot));
        }
        respond(true, value, null);
    }

    private void diagnostics(Context context) throws JSONException {
        String report = P2pVpnService.debugDiagnosticReport();
        JSONObject value = new JSONObject();
        value.put("service_ready", report != null);
        if (report == null) {
            enqueueService(context, "ensure", null);
        } else {
            value.put("report", new JSONObject(report));
        }
        respond(true, value, null);
    }

    private void connect(Context context) throws JSONException {
        if (!LocalNetworkPermission.isGranted(context)) {
            respond(false, null, "local_network_permission_required");
            return;
        }
        if (VpnService.prepare(context) != null) {
            respond(false, null, "vpn_permission_required");
            return;
        }
        startService(context, P2pVpnService.ACTION_CONNECT);
        accepted("connect");
    }

    private void createProfile(Context context, Intent intent) throws JSONException {
        String network = requiredExtra(intent, "network", 128);
        String bootstrapPeerId = optionalExtra(intent, "bootstrap_peer_id", 256);
        String bootstrapAddress = optionalExtra(intent, "bootstrap_address", 1024);
        String kademliaProtocol = optionalExtra(intent, "kademlia_protocol", 128);
        String packetQuicListen =
                optionalProfileExtra(
                        intent,
                        "packet_quic_listen",
                        P2pVpnService.DEBUG_PACKET_QUIC_ENDPOINT_MAX_LENGTH);
        String packetQuicExternalEndpoint =
                optionalProfileExtra(
                        intent,
                        "packet_quic_external_endpoint",
                        P2pVpnService.DEBUG_PACKET_QUIC_ENDPOINT_MAX_LENGTH);
        String relayReservation =
                optionalProfileExtra(
                        intent,
                        "relay_reservation",
                        P2pVpnService.DEBUG_RELAY_RESERVATION_MAX_LENGTH);
        boolean customBootstrap =
                bootstrapPeerId != null || bootstrapAddress != null || kademliaProtocol != null;
        boolean packetQuic = packetQuicListen != null || packetQuicExternalEndpoint != null;
        if (!customBootstrap && !packetQuic && relayReservation == null) {
            enqueue(context, "create-profile", network);
            return;
        }
        if (bootstrapPeerId == null || bootstrapAddress == null || kademliaProtocol == null) {
            throw new IllegalArgumentException("incomplete_bootstrap");
        }
        try {
            P2pVpnService.validateDebugE2ePaths(
                    packetQuicListen, packetQuicExternalEndpoint, relayReservation);
        } catch (P2pVpnException error) {
            throw new IllegalArgumentException("invalid_e2e_paths", error);
        }
        JSONObject settings = new JSONObject();
        settings.put("network", network);
        settings.put("bootstrap_peer_id", bootstrapPeerId);
        settings.put("bootstrap_address", bootstrapAddress);
        settings.put("kademlia_protocol", kademliaProtocol);
        if (packetQuicListen != null) {
            settings.put("packet_quic_listen", packetQuicListen);
            settings.put("packet_quic_external_endpoint", packetQuicExternalEndpoint);
        }
        if (relayReservation != null) {
            settings.put("relay_reservation", relayReservation);
        }
        enqueueService(context, "create-e2e-profile", settings.toString());
        accepted("create-profile");
    }

    private void enqueue(Context context, String command, String value) throws JSONException {
        enqueueService(context, command, value);
        accepted(command);
    }

    private void accepted(String command) throws JSONException {
        JSONObject value = new JSONObject();
        value.put("accepted", true);
        value.put("command", command);
        respond(true, value, null);
    }

    private static void enqueueService(Context context, String command, String value) {
        Intent service = new Intent(context, P2pVpnService.class);
        service.setAction(P2pVpnService.ACTION_DEBUG_COMMAND);
        service.putExtra(P2pVpnService.EXTRA_DEBUG_COMMAND, command);
        if (value != null) {
            service.putExtra(P2pVpnService.EXTRA_DEBUG_VALUE, value);
        }
        context.startService(service);
    }

    private static void startService(Context context, String action) {
        Intent service = new Intent(context, P2pVpnService.class);
        service.setAction(action);
        if (Build.VERSION.SDK_INT >= 26 && P2pVpnService.ACTION_CONNECT.equals(action)) {
            context.startForegroundService(service);
        } else {
            context.startService(service);
        }
    }

    private static JSONObject snapshotJson(P2pVpnService.Snapshot snapshot) throws JSONException {
        JSONObject value = new JSONObject();
        value.put("has_profile", snapshot.hasProfile);
        value.put("profile_stored", snapshot.profileStored);
        value.put("profile_unreadable", snapshot.profileUnreadable);
        value.put("connected", snapshot.connected);
        value.put("connection_requested", snapshot.connectionRequested);
        value.put("always_on", snapshot.alwaysOn);
        value.put("lockdown", snapshot.lockdown);
        value.put("busy", snapshot.busy);
        value.put("network_name", nullable(snapshot.networkName));
        value.put("hostname", nullable(snapshot.hostname));
        value.put("peer_id", nullable(snapshot.peerId));
        value.put("addresses", new JSONArray(snapshot.addresses));
        value.put("connection_detail", snapshot.connectionDetail);
        value.put("peer_detail", snapshot.peerDetail);
        value.put("pairing_detail", snapshot.pairingDetail);
        value.put("runtime_generation", snapshot.runtimeGeneration);

        UnderlayTracker.Snapshot underlaySnapshot = snapshot.underlay;
        JSONObject underlay = new JSONObject();
        underlay.put("kind", underlaySnapshot.kind);
        underlay.put("validated", underlaySnapshot.validated);
        underlay.put("available_networks", underlaySnapshot.availableNetworks);
        underlay.put("selection_changes", underlaySnapshot.selectionChanges);
        underlay.put("selected_losses", underlaySnapshot.selectedLosses);
        underlay.put("recoveries", underlaySnapshot.recoveries);
        underlay.put("runtime_recovery_requests", snapshot.runtimeNetworkChangeRequests);
        underlay.put("runtime_recovery_failures", snapshot.runtimeNetworkChangeFailures);
        value.put("underlay", underlay);

        RuntimeSummary summary = snapshot.runtimeSummary;
        JSONObject paths = new JSONObject();
        paths.put("connected_peers", summary.connectedPeers);
        paths.put("direct_udp_datagram", summary.directUdpDatagramPaths);
        paths.put("direct_quic_datagram", summary.directQuicDatagramPaths);
        paths.put("direct_quic_stream", summary.directQuicStreamPaths);
        paths.put("direct_tcp_stream", summary.directTcpStreamPaths);
        paths.put("relay", summary.relayPaths);
        paths.put("public_routing_peers", summary.publicRoutingPeers);
        paths.put("packet_plane_quic_sessions", summary.packetPlaneQuicSessions);
        paths.put("outbound_quic_datagram_packets", summary.outboundQuicDatagramPackets);
        paths.put("outbound_direct_tcp_stream_packets", summary.outboundDirectTcpStreamPackets);
        paths.put("promotions_to_direct", summary.pathPromotionsToDirect);
        value.put("paths", paths);

        JSONObject pairing = new JSONObject();
        pairing.put("code", nullable(snapshot.pairingCode));
        pairing.put("candidate_peer", nullable(snapshot.candidatePeer));
        pairing.put("candidate_fingerprint", nullable(snapshot.candidateFingerprint));
        pairing.put("candidate_hostname", nullable(snapshot.candidateHostname));
        pairing.put("candidate_vpn_ip", nullable(snapshot.candidateVpnIp));
        value.put("pairing", pairing);
        return value;
    }

    private static String requiredExtra(Intent intent, String name, int maximumLength) {
        String value = intent.getStringExtra(name);
        if (value == null || value.trim().isEmpty() || value.length() > maximumLength) {
            throw new IllegalArgumentException("invalid_" + name);
        }
        return value.trim();
    }

    private static String optionalExtra(Intent intent, String name, int maximumLength) {
        String value = intent.getStringExtra(name);
        if (value == null || value.trim().isEmpty()) {
            return null;
        }
        if (value.length() > maximumLength) {
            throw new IllegalArgumentException("invalid_" + name);
        }
        return value.trim();
    }

    private static String optionalProfileExtra(Intent intent, String name, int maximumLength) {
        if (!intent.hasExtra(name)) {
            return null;
        }
        try {
            return P2pVpnService.boundedOptionalDebugSetting(
                    intent.getStringExtra(name), name, maximumLength);
        } catch (P2pVpnException error) {
            throw new IllegalArgumentException("invalid_" + name, error);
        }
    }

    private static Object nullable(String value) {
        return value == null ? JSONObject.NULL : value;
    }

    private void respond(boolean ok, JSONObject value, String error) {
        try {
            JSONObject response = new JSONObject();
            response.put("schema_version", SCHEMA_VERSION);
            response.put("ok", ok);
            if (ok) {
                response.put("value", value == null ? new JSONObject() : value);
            } else {
                response.put("error", error == null ? "automation_internal_error" : error);
            }
            String encoded =
                    Base64.encodeToString(
                            response.toString().getBytes(StandardCharsets.UTF_8), Base64.NO_WRAP);
            setResultCode(ok ? Activity.RESULT_OK : Activity.RESULT_CANCELED);
            setResultData(encoded);
        } catch (JSONException ignored) {
            setResultCode(Activity.RESULT_CANCELED);
            setResultData(
                    "eyJzY2hlbWFfdmVyc2lvbiI6MSwib2siOmZhbHNlLCJlcnJvciI6ImVuY29kaW5nX2ZhaWxlZCJ9");
        }
    }
}
