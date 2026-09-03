package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertThrows;
import static org.junit.Assert.assertTrue;

import java.util.Optional;
import org.json.JSONArray;
import org.json.JSONObject;
import org.junit.Test;

public final class PeerSnapshotTest {
    @Test
    public void parsesValidSnapshotIntoStableTypedValues() throws Exception {
        JSONObject local = localPeer("localPeer", "android-phone");
        JSONObject remote = connectedPeer("remotePeer", "runner-one", "10.42.0.2", "fd00::2");
        remote.getJSONArray("membership_sources").put("peer_configuration");
        remote.put(
                "membership",
                new JSONObject()
                        .put("state", "active")
                        .put(
                                "effective_inviter",
                                new JSONObject()
                                        .put("peer_id", "currentInviter")
                                        .put("hostname", "current-host"))
                        .put(
                                "original_inviter",
                                new JSONObject().put("peer_id", "originalInviter"))
                        .put("admitted_at_unix_seconds", 1_788_290_900L)
                        .put("original_admitted_at_unix_seconds", 1_788_290_000L)
                        .put("state_changed_at_unix_seconds", 1_788_290_900L));
        remote.put("future_metric", 12);
        JSONObject encoded = snapshot(new JSONArray().put(local).put(remote), 2, 2, false);

        PeerSnapshot parsed = parse(encoded);

        assertEquals(1_788_291_000L, parsed.observedAtUnixSeconds);
        assertEquals(2, parsed.totalPeers);
        assertEquals(2, parsed.returnedPeers);
        assertFalse(parsed.truncated);
        assertEquals("remotePeer", parsed.peers.get(1).peerId);
        assertEquals("runner-one", parsed.peers.get(1).hostnames.get(0));
        assertEquals("10.42.0.2", parsed.peers.get(1).ipv4.get(0));
        assertEquals("fd00::2", parsed.peers.get(1).ipv6.get(0));
        assertEquals(PeerSnapshot.ConnectionState.CONNECTED, parsed.peers.get(1).connectionState);
        assertEquals(
                Optional.of(PeerSnapshot.PathKind.DIRECT_QUIC_STREAM),
                parsed.peers.get(1).selectedPath);
        assertEquals(
                Optional.of(PeerSnapshot.PathOrigin.MDNS), parsed.peers.get(1).pathOrigin);
        assertEquals(
                PeerSnapshot.MembershipSource.SIGNED_MEMBERSHIP,
                parsed.peers.get(1).membershipSources.get(0));
        assertEquals(
                PeerSnapshot.MembershipSource.PEER_CONFIGURATION,
                parsed.peers.get(1).membershipSources.get(1));
        PeerSnapshot.Membership membership = parsed.peers.get(1).membership.get();
        assertEquals(PeerSnapshot.MembershipState.ACTIVE, membership.state);
        assertEquals("currentInviter", membership.effectiveInviter.get().peerId);
        assertEquals("current-host", membership.effectiveInviter.get().hostname.get());
        assertEquals("originalInviter", membership.originalInviter.get().peerId);
        assertEquals(Optional.of(1_788_290_900L), membership.admittedAtUnixSeconds);
        assertEquals(
                Optional.of(1_788_290_000L), membership.originalAdmittedAtUnixSeconds);
    }

    @Test
    public void missingNullAndUnknownVersionsAreUnavailable() throws Exception {
        JSONObject parent = new JSONObject();
        assertFalse(PeerSnapshot.parseOptional(parent, "peer_snapshot").isPresent());

        parent.put("peer_snapshot", JSONObject.NULL);
        assertFalse(PeerSnapshot.parseOptional(parent, "peer_snapshot").isPresent());

        parent.put("peer_snapshot", new JSONObject().put("schema_version", 2));
        assertFalse(PeerSnapshot.parseOptional(parent, "peer_snapshot").isPresent());
    }

    @Test
    public void acceptsMaximumPeerAndPerPeerCollectionBounds() throws Exception {
        JSONArray peers = new JSONArray();
        for (int index = 0; index < PeerSnapshot.MAX_PEERS; index++) {
            JSONObject peer = disconnectedPeer("peer" + index, "peer-" + index);
            if (index == 0) {
                peer = localPeer("peer0", "peer-0");
            }
            peers.put(peer);
        }
        JSONObject finalPeer = peers.getJSONObject(PeerSnapshot.MAX_PEERS - 1);
        finalPeer.put("hostnames", strings("host-a", "host-b", "host-c", "host-d"));
        finalPeer.put(
                "ipv4",
                strings(
                        "10.0.0.1",
                        "10.0.0.2",
                        "10.0.0.3",
                        "10.0.0.4",
                        "10.0.0.5",
                        "10.0.0.6",
                        "10.0.0.7",
                        "10.0.0.8"));
        finalPeer.put(
                "ipv6",
                strings(
                        "fd00::1",
                        "fd00::2",
                        "fd00::3",
                        "fd00::4",
                        "fd00::5",
                        "fd00::6",
                        "fd00::7",
                        "fd00::8"));

        PeerSnapshot parsed =
                parse(snapshot(peers, PeerSnapshot.MAX_PEERS, PeerSnapshot.MAX_PEERS, false));

        assertEquals(PeerSnapshot.MAX_PEERS, parsed.peers.size());
        assertEquals(4, parsed.peers.get(PeerSnapshot.MAX_PEERS - 1).hostnames.size());
        assertEquals(8, parsed.peers.get(PeerSnapshot.MAX_PEERS - 1).ipv4.size());
        assertEquals(8, parsed.peers.get(PeerSnapshot.MAX_PEERS - 1).ipv6.size());
    }

    @Test
    public void rejectsPeerAndPerPeerCollectionOverflows() throws Exception {
        JSONArray peers = new JSONArray();
        for (int index = 0; index <= PeerSnapshot.MAX_PEERS; index++) {
            peers.put(disconnectedPeer("peer" + index, "peer-" + index));
        }
        assertMalformed(snapshot(peers, peers.length(), peers.length(), false));

        JSONObject peer = disconnectedPeer("remotePeer", "runner-one");
        peer.put("hostnames", strings("a", "b", "c", "d", "e"));
        assertMalformed(singlePeerSnapshot(peer));

        peer = disconnectedPeer("remotePeer", "runner-one");
        peer.put("ipv4", sequentialIpv4(9));
        assertMalformed(singlePeerSnapshot(peer));

        peer = disconnectedPeer("remotePeer", "runner-one");
        peer.put("ipv6", sequentialIpv6(9));
        assertMalformed(singlePeerSnapshot(peer));
    }

    @Test
    public void rejectsMalformedRequiredFieldsAndCountInvariants() throws Exception {
        JSONObject missingTime = singlePeerSnapshot(disconnectedPeer("remotePeer", "runner-one"));
        missingTime.remove("observed_at_unix_seconds");
        assertMalformed(missingTime);

        JSONObject stringCount = singlePeerSnapshot(disconnectedPeer("remotePeer", "runner-one"));
        stringCount.put("returned_peers", "1");
        assertMalformed(stringCount);

        JSONObject mismatched = singlePeerSnapshot(disconnectedPeer("remotePeer", "runner-one"));
        mismatched.put("returned_peers", 0);
        assertMalformed(mismatched);

        JSONObject badTruncation =
                snapshot(
                        new JSONArray().put(disconnectedPeer("remotePeer", "runner-one")),
                        2,
                        1,
                        false);
        assertMalformed(badTruncation);

        JSONObject nonObjectPeer = snapshot(new JSONArray().put("peer"), 1, 1, false);
        assertMalformed(nonObjectPeer);

        JSONObject duplicatePeers =
                snapshot(
                        new JSONArray()
                                .put(disconnectedPeer("remotePeer", "runner-one"))
                                .put(disconnectedPeer("remotePeer", "runner-two")),
                        2,
                        2,
                        false);
        assertMalformed(duplicatePeers);
    }

    @Test
    public void rejectsUnknownEnumsAndInconsistentPathState() throws Exception {
        JSONObject peer = disconnectedPeer("remotePeer", "runner-one");
        peer.put("membership_sources", strings("discovered_somehow"));
        assertMalformed(singlePeerSnapshot(peer));

        peer = disconnectedPeer("remotePeer", "runner-one");
        peer.put("connection_state", "online");
        assertMalformed(singlePeerSnapshot(peer));

        peer = disconnectedPeer("remotePeer", "runner-one");
        peer.put("selected_path", "direct_quic_stream");
        peer.put("path_origin", "mdns");
        assertMalformed(singlePeerSnapshot(peer));

        peer = connectedPeer("remotePeer", "runner-one", "10.42.0.2", "fd00::2");
        peer.remove("path_origin");
        assertMalformed(singlePeerSnapshot(peer));

        peer = connectedPeer("remotePeer", "runner-one", "10.42.0.2", "fd00::2");
        peer.put("selected_path", "relay-through-peer-id");
        assertMalformed(singlePeerSnapshot(peer));
    }

    @Test
    public void rejectsMalformedMembershipProvenance() throws Exception {
        JSONObject peer = disconnectedPeer("remotePeer", "runner-one");
        peer.put("membership", new JSONObject().put("state", "removed"));
        assertMalformed(singlePeerSnapshot(peer));

        peer = disconnectedPeer("remotePeer", "runner-one");
        peer.put(
                "membership",
                new JSONObject()
                        .put("state", "active")
                        .put("effective_inviter", "not-an-object"));
        assertMalformed(singlePeerSnapshot(peer));

        peer = disconnectedPeer("remotePeer", "runner-one");
        peer.put(
                "membership",
                new JSONObject()
                        .put("state", "active")
                        .put(
                                "effective_inviter",
                                new JSONObject().put("peer_id", "invalid peer")));
        assertMalformed(singlePeerSnapshot(peer));

        peer = disconnectedPeer("remotePeer", "runner-one");
        peer.put(
                "membership",
                new JSONObject()
                        .put("state", "active")
                        .put(
                                "effective_inviter",
                                new JSONObject()
                                        .put("peer_id", "inviterPeer")
                                        .put("hostname", "Invalid-Host")));
        assertMalformed(singlePeerSnapshot(peer));

        peer = disconnectedPeer("remotePeer", "runner-one");
        peer.put(
                "membership",
                new JSONObject()
                        .put("state", "revoked")
                        .put("state_changed_at_unix_seconds", -1));
        assertMalformed(singlePeerSnapshot(peer));
    }

    @Test
    public void rejectsInvalidOrDuplicateIdentityAndAddressValues() throws Exception {
        JSONObject peer = disconnectedPeer("remote peer", "runner-one");
        assertMalformed(singlePeerSnapshot(peer));

        peer = disconnectedPeer("remotePeer", "Runner-One");
        assertMalformed(singlePeerSnapshot(peer));

        peer = disconnectedPeer("remotePeer", "runner-one");
        peer.put("hostnames", strings("runner-one", "runner-one"));
        assertMalformed(singlePeerSnapshot(peer));

        peer = disconnectedPeer("remotePeer", "runner-one");
        peer.put("ipv4", strings("010.42.0.2"));
        assertMalformed(singlePeerSnapshot(peer));

        peer = disconnectedPeer("remotePeer", "runner-one");
        peer.put("ipv6", strings("FD00::2"));
        assertMalformed(singlePeerSnapshot(peer));

        peer = disconnectedPeer("remotePeer", "runner-one");
        peer.put("ipv4", strings("fd00::2"));
        assertMalformed(singlePeerSnapshot(peer));
    }

    @Test
    public void parsedCollectionsDoNotRetainMutableJsonOrAllowMutation() throws Exception {
        JSONObject peer = connectedPeer("remotePeer", "runner-one", "10.42.0.2", "fd00::2");
        JSONObject encoded = singlePeerSnapshot(peer);
        PeerSnapshot parsed = parse(encoded);

        peer.getJSONArray("hostnames").put("changed-after-parse");
        encoded.getJSONArray("peers").put(disconnectedPeer("laterPeer", "later-peer"));

        assertEquals(1, parsed.peers.size());
        assertEquals(1, parsed.peers.get(0).hostnames.size());
        assertThrows(
                UnsupportedOperationException.class,
                () -> parsed.peers.add(parsed.peers.get(0)));
        assertThrows(
                UnsupportedOperationException.class,
                () -> parsed.peers.get(0).hostnames.add("another"));
        assertThrows(
                UnsupportedOperationException.class,
                () -> parsed.peers.get(0).ipv4.add("10.42.0.3"));
        assertThrows(
                UnsupportedOperationException.class,
                () ->
                        parsed.peers
                                .get(0)
                                .membershipSources
                                .add(PeerSnapshot.MembershipSource.PEER_CONFIGURATION));
    }

    @Test
    public void ignoresHarmlessExtensionsButRejectsSecretsAndTopologyIdentifiers()
            throws Exception {
        JSONObject peer = disconnectedPeer("remotePeer", "runner-one");
        peer.put("display_hint", "nearby");
        JSONObject encoded = singlePeerSnapshot(peer);
        encoded.put("producer_revision", 7);
        assertEquals(1, parse(encoded).peers.size());

        encoded = singlePeerSnapshot(disconnectedPeer("remotePeer", "runner-one"));
        encoded.put("private_key", "must-not-cross-jni");
        assertMalformed(encoded);

        peer = disconnectedPeer("remotePeer", "runner-one");
        peer.put("multiaddrs", strings("/ip4/192.0.2.1/tcp/4001"));
        assertMalformed(singlePeerSnapshot(peer));

        peer = disconnectedPeer("remotePeer", "runner-one");
        peer.put("debug", new JSONObject().put("relay_peer_id", "relayPeer"));
        assertMalformed(singlePeerSnapshot(peer));
    }

    @Test
    public void rejectsOversizedKnownSchemaButDoesNotInspectFutureSchema() throws Exception {
        JSONObject oversized = singlePeerSnapshot(disconnectedPeer("remotePeer", "runner-one"));
        oversized.put("future_padding", "x".repeat(PeerSnapshot.MAX_ENCODED_BYTES));
        assertMalformed(oversized);

        JSONObject future =
                new JSONObject()
                        .put("schema_version", PeerSnapshot.SCHEMA_VERSION + 1)
                        .put("private_key", "ignored-with-entire-future-schema");
        assertFalse(parseOptional(future).isPresent());
    }

    @Test
    public void rejectsExcessiveExtensionNestingWithoutRecursion() throws Exception {
        JSONObject encoded = singlePeerSnapshot(disconnectedPeer("remotePeer", "runner-one"));
        JSONObject nested = encoded;
        for (int index = 0; index < 4_100; index++) {
            JSONObject child = new JSONObject();
            nested.put("extension", child);
            nested = child;
        }

        assertMalformed(encoded);
    }

    private static PeerSnapshot parse(JSONObject snapshot) throws Exception {
        return parseOptional(snapshot).orElseThrow();
    }

    private static Optional<PeerSnapshot> parseOptional(JSONObject snapshot) throws Exception {
        return PeerSnapshot.parseOptional(
                new JSONObject().put("peer_snapshot", snapshot), "peer_snapshot");
    }

    private static void assertMalformed(JSONObject snapshot) {
        assertThrows(P2pVpnException.class, () -> parseOptional(snapshot));
    }

    private static JSONObject snapshot(
            JSONArray peers, int totalPeers, int returnedPeers, boolean truncated)
            throws Exception {
        return new JSONObject()
                .put("schema_version", PeerSnapshot.SCHEMA_VERSION)
                .put("observed_at_unix_seconds", 1_788_291_000L)
                .put("total_peers", totalPeers)
                .put("returned_peers", returnedPeers)
                .put("truncated", truncated)
                .put("peers", peers);
    }

    private static JSONObject singlePeerSnapshot(JSONObject peer) throws Exception {
        return snapshot(new JSONArray().put(peer), 1, 1, false);
    }

    private static JSONObject localPeer(String peerId, String hostname) throws Exception {
        return basePeer(peerId, hostname)
                .put("local", true)
                .put("membership_sources", strings("local_configuration"))
                .put("connection_state", "local");
    }

    private static JSONObject disconnectedPeer(String peerId, String hostname) throws Exception {
        return basePeer(peerId, hostname)
                .put("local", false)
                .put("membership_sources", strings("signed_membership"))
                .put("connection_state", "disconnected");
    }

    private static JSONObject connectedPeer(
            String peerId, String hostname, String ipv4, String ipv6) throws Exception {
        return disconnectedPeer(peerId, hostname)
                .put("ipv4", strings(ipv4))
                .put("ipv6", strings(ipv6))
                .put("connection_state", "connected")
                .put("selected_path", "direct_quic_stream")
                .put("path_origin", "mdns");
    }

    private static JSONObject basePeer(String peerId, String hostname) throws Exception {
        return new JSONObject()
                .put("peer_id", peerId)
                .put("hostnames", strings(hostname))
                .put("ipv4", new JSONArray())
                .put("ipv6", new JSONArray());
    }

    private static JSONArray strings(String... values) {
        JSONArray result = new JSONArray();
        for (String value : values) {
            result.put(value);
        }
        return result;
    }

    private static JSONArray sequentialIpv4(int count) {
        JSONArray result = new JSONArray();
        for (int index = 1; index <= count; index++) {
            result.put("10.0.0." + index);
        }
        return result;
    }

    private static JSONArray sequentialIpv6(int count) {
        JSONArray result = new JSONArray();
        for (int index = 1; index <= count; index++) {
            result.put("fd00::" + Integer.toHexString(index));
        }
        return result;
    }
}
