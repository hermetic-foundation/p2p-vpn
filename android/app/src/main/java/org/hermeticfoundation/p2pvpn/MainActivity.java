package org.hermeticfoundation.p2pvpn;

import android.Manifest;
import android.app.Activity;
import android.content.ClipData;
import android.content.ClipDescription;
import android.content.ClipboardManager;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.ServiceConnection;
import android.content.pm.PackageManager;
import android.net.VpnService;
import android.os.Build;
import android.os.Bundle;
import android.os.IBinder;
import android.os.PersistableBundle;
import android.view.View;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.TextView;
import android.widget.Toast;

public final class MainActivity extends Activity implements P2pVpnService.Listener {
    private static final int VPN_PERMISSION_REQUEST = 100;
    private static final int NOTIFICATION_PERMISSION_REQUEST = 101;

    private LinearLayout profileSetup;
    private LinearLayout generatedCodeGroup;
    private LinearLayout candidateGroup;
    private EditText networkName;
    private EditText joinCode;
    private EditText assignedHostname;
    private TextView identity;
    private TextView generatedCode;
    private TextView candidateDetails;
    private TextView status;
    private Button createProfile;
    private Button connect;
    private Button disconnect;
    private Button openPairing;
    private Button joinPairing;
    private Button approvePairing;
    private Button rejectPairing;

    private P2pVpnService.LocalBinder binder;
    private P2pVpnService.Snapshot latestSnapshot;
    private String displayedCandidatePeer;
    private boolean bound;

    private final ServiceConnection serviceConnection =
            new ServiceConnection() {
                @Override
                public void onServiceConnected(ComponentName name, IBinder service) {
                    binder = (P2pVpnService.LocalBinder) service;
                    bound = true;
                    binder.addListener(MainActivity.this);
                }

                @Override
                public void onServiceDisconnected(ComponentName name) {
                    if (binder != null) {
                        binder.removeListener(MainActivity.this);
                    }
                    binder = null;
                    bound = false;
                    showLocalStatus("VPN service stopped");
                }
            };

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.activity_main);
        bindViews();
        bindActions();
        requestNotificationPermission();
    }

    @Override
    protected void onStart() {
        super.onStart();
        Intent service = new Intent(this, P2pVpnService.class);
        bindService(service, serviceConnection, Context.BIND_AUTO_CREATE);
    }

    @Override
    protected void onStop() {
        if (bound) {
            binder.removeListener(this);
            unbindService(serviceConnection);
            binder = null;
            bound = false;
        }
        super.onStop();
    }

    @Override
    public void onSnapshot(P2pVpnService.Snapshot snapshot) {
        latestSnapshot = snapshot;
        profileSetup.setVisibility(snapshot.hasProfile ? View.GONE : View.VISIBLE);
        if (snapshot.hasProfile) {
            identity.setText(
                    getString(R.string.identity_format, snapshot.networkName, snapshot.peerId));
        } else {
            identity.setText(R.string.identity_unavailable);
        }

        createProfile.setEnabled(!snapshot.hasProfile && !snapshot.busy);
        connect.setEnabled(snapshot.hasProfile && !snapshot.connected && !snapshot.busy);
        disconnect.setEnabled(snapshot.connected);
        openPairing.setEnabled(snapshot.connected && !snapshot.busy);
        joinPairing.setEnabled(snapshot.connected && !snapshot.busy);

        boolean hasCode = snapshot.pairingCode != null && !snapshot.pairingCode.isEmpty();
        generatedCodeGroup.setVisibility(hasCode ? View.VISIBLE : View.GONE);
        generatedCode.setText(hasCode ? snapshot.pairingCode : "");

        boolean hasCandidate = snapshot.candidatePeer != null;
        candidateGroup.setVisibility(hasCandidate ? View.VISIBLE : View.GONE);
        approvePairing.setEnabled(hasCandidate && snapshot.connected);
        rejectPairing.setEnabled(hasCandidate && snapshot.connected);
        if (hasCandidate) {
            StringBuilder details = new StringBuilder(snapshot.candidatePeer);
            if (snapshot.candidateFingerprint != null) {
                details.append("\n").append(snapshot.candidateFingerprint);
            }
            if (snapshot.candidateVpnIp != null) {
                details.append("\nRequested IP: ").append(snapshot.candidateVpnIp);
            }
            candidateDetails.setText(details.toString());
            if (!snapshot.candidatePeer.equals(displayedCandidatePeer)) {
                displayedCandidatePeer = snapshot.candidatePeer;
                assignedHostname.setText(
                        snapshot.candidateHostname == null ? "" : snapshot.candidateHostname);
            }
        } else {
            displayedCandidatePeer = null;
            candidateDetails.setText("");
        }

        status.setText(
                getString(
                        R.string.status_format,
                        snapshot.connectionDetail,
                        snapshot.peerDetail,
                        snapshot.pairingDetail));
    }

    @Override
    @SuppressWarnings("deprecation")
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode == VPN_PERMISSION_REQUEST) {
            if (resultCode == RESULT_OK) {
                startVpnService();
            } else {
                showLocalStatus("VPN permission was not granted");
            }
        }
    }

    private void bindViews() {
        profileSetup = findViewById(R.id.profile_setup);
        generatedCodeGroup = findViewById(R.id.generated_code_group);
        candidateGroup = findViewById(R.id.candidate_group);
        networkName = findViewById(R.id.network_name);
        joinCode = findViewById(R.id.join_code);
        assignedHostname = findViewById(R.id.assigned_hostname);
        identity = findViewById(R.id.identity);
        generatedCode = findViewById(R.id.generated_code);
        candidateDetails = findViewById(R.id.candidate_details);
        status = findViewById(R.id.status);
        createProfile = findViewById(R.id.create_profile);
        connect = findViewById(R.id.connect);
        disconnect = findViewById(R.id.disconnect);
        openPairing = findViewById(R.id.open_pairing);
        joinPairing = findViewById(R.id.join_pairing);
        approvePairing = findViewById(R.id.approve_pairing);
        rejectPairing = findViewById(R.id.reject_pairing);
    }

    private void bindActions() {
        createProfile.setOnClickListener(
                view -> {
                    if (binder == null) {
                        showLocalStatus("VPN service is not ready");
                        return;
                    }
                    String name = networkName.getText().toString().trim();
                    if (name.isEmpty()) {
                        networkName.setError(getString(R.string.network_name_hint));
                        return;
                    }
                    binder.createProfile(name);
                });
        connect.setOnClickListener(view -> requestVpnConnection());
        disconnect.setOnClickListener(view -> stopVpnService());
        openPairing.setOnClickListener(
                view -> {
                    if (binder != null) {
                        binder.openPairing();
                    }
                });
        joinPairing.setOnClickListener(
                view -> {
                    if (binder != null) {
                        binder.joinPairing(joinCode.getText().toString());
                    }
                });
        approvePairing.setOnClickListener(
                view -> {
                    if (binder != null) {
                        binder.approvePairing(assignedHostname.getText().toString());
                    }
                });
        rejectPairing.setOnClickListener(
                view -> {
                    if (binder != null) {
                        binder.rejectPairing();
                    }
                });
        findViewById(R.id.copy_code)
                .setOnClickListener(
                        view -> {
                            CharSequence code = generatedCode.getText();
                            if (code.length() == 0) {
                                return;
                            }
                            ClipboardManager clipboard =
                                    getSystemService(ClipboardManager.class);
                            ClipData clip = ClipData.newPlainText("p2p-vpn code", code);
                            if (Build.VERSION.SDK_INT >= 33) {
                                PersistableBundle extras = new PersistableBundle();
                                extras.putBoolean(ClipDescription.EXTRA_IS_SENSITIVE, true);
                                clip.getDescription().setExtras(extras);
                            }
                            clipboard.setPrimaryClip(clip);
                            Toast.makeText(
                                            this,
                                            R.string.pairing_code_copied,
                                            Toast.LENGTH_SHORT)
                                    .show();
                        });
    }

    @SuppressWarnings("deprecation")
    private void requestVpnConnection() {
        if (latestSnapshot == null || !latestSnapshot.hasProfile) {
            showLocalStatus("Create a profile before connecting");
            return;
        }
        Intent permission = VpnService.prepare(this);
        if (permission == null) {
            startVpnService();
        } else {
            startActivityForResult(permission, VPN_PERMISSION_REQUEST);
        }
    }

    private void startVpnService() {
        Intent intent = new Intent(this, P2pVpnService.class);
        intent.setAction(P2pVpnService.ACTION_CONNECT);
        startForegroundService(intent);
    }

    private void stopVpnService() {
        Intent intent = new Intent(this, P2pVpnService.class);
        intent.setAction(P2pVpnService.ACTION_DISCONNECT);
        startService(intent);
    }

    private void requestNotificationPermission() {
        if (Build.VERSION.SDK_INT >= 33
                && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)
                        != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(
                    new String[] {Manifest.permission.POST_NOTIFICATIONS},
                    NOTIFICATION_PERMISSION_REQUEST);
        }
    }

    private void showLocalStatus(String message) {
        status.setText(message);
    }
}
