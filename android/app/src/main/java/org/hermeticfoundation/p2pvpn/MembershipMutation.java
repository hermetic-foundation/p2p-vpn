package org.hermeticfoundation.p2pvpn;

import org.json.JSONException;
import org.json.JSONObject;

final class MembershipMutation {
    private MembershipMutation() {}

    static Result revoke(String networkId, String memberPeer) throws P2pVpnException {
        String normalizedNetworkId = ProfileCollection.Entry.normalizeNetworkId(networkId);
        String normalizedMemberPeer = requirePeerId(memberPeer);
        Result result =
                Result.from(
                        NativeResponse.objectValue(
                                NativeBridge.nativeRevokeMember(
                                        normalizedNetworkId, normalizedMemberPeer)));
        if (result.resigned || !normalizedMemberPeer.equals(result.memberPeer)) {
            throw new P2pVpnException("Native member revocation returned an inconsistent result");
        }
        return result;
    }

    static Result resign(String networkId) throws P2pVpnException {
        String normalizedNetworkId = ProfileCollection.Entry.normalizeNetworkId(networkId);
        Result result =
                Result.from(
                        NativeResponse.objectValue(
                                NativeBridge.nativeResignMembership(normalizedNetworkId)));
        if (!result.resigned) {
            throw new P2pVpnException("Native membership resignation returned an inconsistent result");
        }
        return result;
    }

    private static String requirePeerId(String value) throws P2pVpnException {
        if (value == null || value.isEmpty() || value.length() > PeerSnapshot.MAX_PEER_ID_BYTES) {
            throw new P2pVpnException("Membership peer ID is invalid");
        }
        for (int index = 0; index < value.length(); index++) {
            char character = value.charAt(index);
            if (!((character >= 'a' && character <= 'z')
                    || (character >= 'A' && character <= 'Z')
                    || (character >= '0' && character <= '9'))) {
                throw new P2pVpnException("Membership peer ID is invalid");
            }
        }
        return value;
    }

    static final class Result {
        final String memberPeer;
        final String issuerPeer;
        final long membershipEpoch;
        final long sequence;
        final boolean resigned;

        private Result(
                String memberPeer,
                String issuerPeer,
                long membershipEpoch,
                long sequence,
                boolean resigned) {
            this.memberPeer = memberPeer;
            this.issuerPeer = issuerPeer;
            this.membershipEpoch = membershipEpoch;
            this.sequence = sequence;
            this.resigned = resigned;
        }

        static Result from(JSONObject value) throws P2pVpnException {
            try {
                String memberPeer = requirePeerId(value.getString("member_peer"));
                String issuerPeer = requirePeerId(value.getString("issuer_peer"));
                long membershipEpoch = requireNonNegativeLong(value, "membership_epoch");
                long sequence = requireNonNegativeLong(value, "sequence");
                Object resigned = value.get("resigned");
                if (!(resigned instanceof Boolean)) {
                    throw new P2pVpnException("Membership mutation resignation flag is invalid");
                }
                return new Result(
                        memberPeer,
                        issuerPeer,
                        membershipEpoch,
                        sequence,
                        (Boolean) resigned);
            } catch (JSONException error) {
                throw new P2pVpnException("Membership mutation response is malformed", error);
            }
        }

        private static long requireNonNegativeLong(JSONObject value, String key)
                throws JSONException, P2pVpnException {
            Object encoded = value.get(key);
            if (!(encoded instanceof Byte
                    || encoded instanceof Short
                    || encoded instanceof Integer
                    || encoded instanceof Long)) {
                throw new P2pVpnException("Membership mutation version is invalid");
            }
            long parsed = ((Number) encoded).longValue();
            if (parsed < 0) {
                throw new P2pVpnException("Membership mutation version is invalid");
            }
            return parsed;
        }
    }
}
