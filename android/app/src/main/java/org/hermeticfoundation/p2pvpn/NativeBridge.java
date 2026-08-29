package org.hermeticfoundation.p2pvpn;

final class NativeBridge {
    static {
        System.loadLibrary("p2p_vpn_android");
    }

    private NativeBridge() {}

    static native String nativeCreateProfile(String networkName);

    static native String nativeInspectProfile(String configJson);

    static native String nativeStart(
            String configJson,
            int tunFd,
            String pairingStatePath,
            String membershipStatePath);

    static native String nativeStop();

    static native String nativeStatus();

    static native String nativePairRpc(String requestJson);

    static native String nativeApplyPairingArtifacts(String configJson, String artifactsJson);
}
