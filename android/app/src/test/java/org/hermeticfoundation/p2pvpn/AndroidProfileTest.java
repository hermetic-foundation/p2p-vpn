package org.hermeticfoundation.p2pvpn;

import static org.junit.Assert.assertEquals;

import java.lang.reflect.Constructor;
import java.util.Collections;
import org.junit.Test;

public final class AndroidProfileTest {
    @Test
    public void profileCarriesTheGeneratedHostname() throws Exception {
        Constructor<AndroidProfile> constructor =
                AndroidProfile.class.getDeclaredConstructor(
                        String.class,
                        String.class,
                        String.class,
                        String.class,
                        String.class,
                        int.class,
                        java.util.List.class,
                        java.util.List.class);
        constructor.setAccessible(true);
        AndroidProfile profile =
                constructor.newInstance(
                        "{}",
                        "personal",
                        "android-0123456789abcdef",
                        "12D3KooWPeer",
                        "pv0",
                        1280,
                        Collections.emptyList(),
                        Collections.emptyList());

        assertEquals("android-0123456789abcdef", profile.hostname);
    }
}
