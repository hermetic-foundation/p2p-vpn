package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;

import org.junit.Test;

public final class PairRpcTest {
    @Test
    public void openMatchesInternallyTaggedSerdeShape() {
        assertEquals(
                "{\"method\":\"pair_open\",\"params\":{\"operation_id\":\"op-1\",\"expires_in_seconds\":600}}",
                PairRpc.open("op-1", 600));
    }

    @Test
    public void joinCarriesCodeAndDefaultRequests() {
        assertEquals(
                "{\"method\":\"pair_join\",\"params\":{\"operation_id\":\"op-2\",\"code\":\"ABCD-EFGH-JKLM-NPQR\",\"timeout_seconds\":600,\"requested_vpn_ip\":null,\"requested_routes\":[]}}",
                PairRpc.join("op-2", "ABCD-EFGH-JKLM-NPQR", 600));
    }

    @Test
    public void approvalOmitsAbsentHostnameAndKeepsGrantDefaults() {
        assertEquals(
                "{\"method\":\"pair_approve\",\"params\":{\"operation_id\":\"op-3\",\"approval_id\":\"approval\",\"assigned_vpn_ip\":null,\"granted_routes\":[]}}",
                PairRpc.approve("op-3", "approval", null));
    }

    @Test
    public void approvalIncludesExplicitHostname() {
        assertEquals(
                "{\"method\":\"pair_approve\",\"params\":{\"operation_id\":\"op-3\",\"approval_id\":\"approval\",\"assigned_hostname\":\"runner-1\",\"assigned_vpn_ip\":null,\"granted_routes\":[]}}",
                PairRpc.approve("op-3", "approval", "runner-1"));
    }

    @Test
    public void artifactsMatchesSerdeShape() {
        assertEquals(
                "{\"method\":\"pair_artifacts\",\"params\":{\"operation_id\":\"op-4\"}}",
                PairRpc.artifacts("op-4"));
    }

    @Test
    public void acknowledgementEscapesUntrustedStrings() {
        assertEquals(
                "{\"method\":\"pair_acknowledge\",\"params\":{\"operation_id\":\"op\\\"4\",\"transcript_sha256\":\"digest\\\\value\"}}",
                PairRpc.acknowledge("op\"4", "digest\\value"));
    }
}
