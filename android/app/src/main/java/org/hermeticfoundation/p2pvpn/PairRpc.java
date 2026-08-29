package org.hermeticfoundation.p2pvpn;

import org.json.JSONException;
import org.json.JSONObject;

final class PairRpc {
    private static final int VERSION = 1;

    private PairRpc() {}

    static String open(String operationId, long expiresInSeconds) {
        return request(
                "pair_open",
                "{\"operation_id\":"
                        + JsonStrings.quote(operationId)
                        + ",\"expires_in_seconds\":"
                        + expiresInSeconds
                        + "}");
    }

    static String join(String operationId, String code, long timeoutSeconds) {
        return request(
                "pair_join",
                "{\"operation_id\":"
                        + JsonStrings.quote(operationId)
                        + ",\"code\":"
                        + JsonStrings.quote(code)
                        + ",\"timeout_seconds\":"
                        + timeoutSeconds
                        + ",\"requested_vpn_ip\":null,\"requested_routes\":[]}");
    }

    static String status(String operationId) {
        return request(
                "pair_status",
                "{\"operation_id\":" + JsonStrings.quote(operationId) + "}");
    }

    static String approve(String operationId, String approvalId, String hostname) {
        StringBuilder params =
                new StringBuilder("{\"operation_id\":")
                        .append(JsonStrings.quote(operationId))
                        .append(",\"approval_id\":")
                        .append(JsonStrings.quote(approvalId));
        if (hostname != null && !hostname.isEmpty()) {
            params.append(",\"assigned_hostname\":").append(JsonStrings.quote(hostname));
        }
        params.append(",\"assigned_vpn_ip\":null,\"granted_routes\":[]}");
        return request("pair_approve", params.toString());
    }

    static String reject(String operationId, String approvalId) {
        return request(
                "pair_reject",
                "{\"operation_id\":"
                        + JsonStrings.quote(operationId)
                        + ",\"approval_id\":"
                        + JsonStrings.quote(approvalId)
                        + ",\"reason\":\"declined\"}");
    }

    static String cancel(String operationId) {
        return request(
                "pair_cancel",
                "{\"operation_id\":" + JsonStrings.quote(operationId) + "}");
    }

    static String artifacts(String operationId) {
        return request(
                "pair_artifacts",
                "{\"operation_id\":" + JsonStrings.quote(operationId) + "}");
    }

    static String acknowledge(String operationId, String transcriptSha256) {
        return request(
                "pair_acknowledge",
                "{\"operation_id\":"
                        + JsonStrings.quote(operationId)
                        + ",\"transcript_sha256\":"
                        + JsonStrings.quote(transcriptSha256)
                        + "}");
    }

    static Result call(String request) throws P2pVpnException {
        JSONObject envelope = NativeResponse.objectValue(NativeBridge.nativePairRpc(request));
        try {
            if (envelope.getInt("version") != VERSION) {
                throw new P2pVpnException("Unsupported pairing RPC response version");
            }
            JSONObject outcome = envelope.getJSONObject("outcome");
            String status = outcome.getString("status");
            if ("error".equals(status)) {
                JSONObject error = outcome.getJSONObject("error");
                throw new P2pVpnException(error.optString("message", "Pairing request failed"));
            }
            if (!"ok".equals(status)) {
                throw new P2pVpnException("Pairing RPC returned an invalid outcome");
            }
            JSONObject result = outcome.getJSONObject("result");
            return new Result(result.getString("kind"), result.getJSONObject("value"));
        } catch (JSONException error) {
            throw new P2pVpnException("Pairing RPC returned malformed JSON", error);
        }
    }

    static OperationStatus operationStatus(Result result) throws P2pVpnException {
        if (!"operation_status".equals(result.kind) && !"action_accepted".equals(result.kind)) {
            throw new P2pVpnException("Pairing RPC returned an unexpected result: " + result.kind);
        }
        return OperationStatus.from(result.value);
    }

    private static String request(String method, String params) {
        return "{\"method\":" + JsonStrings.quote(method) + ",\"params\":" + params + "}";
    }

    static final class Result {
        final String kind;
        final JSONObject value;

        Result(String kind, JSONObject value) {
            this.kind = kind;
            this.value = value;
        }
    }

    static final class OperationStatus {
        final String operationId;
        final String phase;
        final String role;
        final boolean artifactsReady;
        final Candidate candidate;
        final String failureMessage;

        private OperationStatus(
                String operationId,
                String phase,
                String role,
                boolean artifactsReady,
                Candidate candidate,
                String failureMessage) {
            this.operationId = operationId;
            this.phase = phase;
            this.role = role;
            this.artifactsReady = artifactsReady;
            this.candidate = candidate;
            this.failureMessage = failureMessage;
        }

        static OperationStatus from(JSONObject value) throws P2pVpnException {
            try {
                Candidate candidate = null;
                JSONObject encodedCandidate = value.optJSONObject("candidate");
                if (encodedCandidate != null) {
                    candidate = Candidate.from(encodedCandidate);
                }
                JSONObject failure = value.optJSONObject("failure");
                return new OperationStatus(
                        value.getString("operation_id"),
                        value.getString("phase"),
                        value.getString("role"),
                        value.optBoolean("artifacts_ready", false),
                        candidate,
                        failure == null ? null : failure.optString("message", "Pairing failed"));
            } catch (JSONException error) {
                throw new P2pVpnException("Pairing status is malformed", error);
            }
        }
    }

    static final class Candidate {
        final String approvalId;
        final String peerId;
        final String fingerprint;
        final String requestedHostname;
        final String requestedVpnIp;

        private Candidate(
                String approvalId,
                String peerId,
                String fingerprint,
                String requestedHostname,
                String requestedVpnIp) {
            this.approvalId = approvalId;
            this.peerId = peerId;
            this.fingerprint = fingerprint;
            this.requestedHostname = requestedHostname;
            this.requestedVpnIp = requestedVpnIp;
        }

        static Candidate from(JSONObject value) throws JSONException {
            return new Candidate(
                    value.getString("approval_id"),
                    value.getString("peer_id"),
                    value.getString("public_key_fingerprint"),
                    nullableString(value, "requested_hostname"),
                    nullableString(value, "requested_vpn_ip"));
        }

        private static String nullableString(JSONObject value, String key) {
            if (value.isNull(key)) {
                return null;
            }
            String result = value.optString(key, null);
            return result == null || result.isEmpty() ? null : result;
        }
    }
}
