package org.hermeticfoundation.p2pvpn;

import android.content.Context;
import android.content.pm.PackageManager;
import android.os.Build;

final class LocalNetworkPermission {
    static final String NAME = "android.permission.ACCESS_LOCAL_NETWORK";
    static final int ENFORCED_API_LEVEL = 37;

    private LocalNetworkPermission() {}

    static boolean isRequired(int deviceApiLevel, int targetApiLevel) {
        return deviceApiLevel >= ENFORCED_API_LEVEL
                && targetApiLevel >= ENFORCED_API_LEVEL;
    }

    static boolean isRequired(Context context) {
        return isRequired(Build.VERSION.SDK_INT, context.getApplicationInfo().targetSdkVersion);
    }

    static boolean isGranted(Context context) {
        return !isRequired(context)
                || context.checkSelfPermission(NAME) == PackageManager.PERMISSION_GRANTED;
    }
}
