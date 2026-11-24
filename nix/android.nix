{
  pkgs,
  sdk,
  src ? ../.,
  version ? "0.1.0",
  androidSdkLicenseAccepted ? false,
  demandLifecycleTrial ? false,
}:

let
  inherit (pkgs) lib;

  pname = "p2p-vpn-android";
  arm64Abi = "arm64-v8a";
  x86_64Abi = "x86_64";
  abis = [
    arm64Abi
    x86_64Abi
  ];

  minimumSdkVersion = "29";
  platformVersion = "37";
  buildToolsVersion = "37.0.0";
  ndkVersion = "28.2.13676358";
  emulatorPlatformVersion = "35";

  androidNixpkgsConfig = {
    allowUnfree = true;
    android_sdk.accept_license = androidSdkLicenseAccepted;
  };

  # Keep Android's unfree toolchain policy local to this build module.
  androidPkgs = import pkgs.path {
    system = pkgs.stdenv.hostPlatform.system;
    config = androidNixpkgsConfig;
  };

  jdk = androidPkgs.jdk17;
  gradle = androidPkgs.gradle_9.override { java = jdk; };
  androidComposition = androidPkgs.androidenv.composeAndroidPackages {
    platformVersions = [ platformVersion ];
    buildToolsVersions = [ buildToolsVersion ];
    includeNDK = true;
    ndkVersions = [ ndkVersion ];
    includeCmake = false;
    includeEmulator = false;
    includeSources = false;
    includeSystemImages = false;
  };
  androidSdk = androidComposition.androidsdk;
  platformTools = androidComposition.platform-tools;
  androidHome = "${androidSdk}/libexec/android-sdk";
  androidNdkRoot = "${androidHome}/ndk/${ndkVersion}";

  androidProject = src + "/android";
  androidProjectPresent = builtins.pathExists androidProject;

  unavailable =
    name: reason:
    pkgs.runCommand name
      {
        passthru = {
          available = false;
          inherit reason;
        };
      }
      ''
        echo ${lib.escapeShellArg reason} >&2
        exit 1
      '';

  sdkPackages = sdk.packages.${pkgs.stdenv.hostPlatform.system};
  variantSuffix = lib.optionalString demandLifecycleTrial "-demand-lifecycle-trial";
  sdkPackage =
    name:
    let
      attr = name + variantSuffix;
    in
    sdkPackages.${attr}
      or (throw "The pinned libp2p-vpn SDK must export packages.${pkgs.stdenv.hostPlatform.system}.${attr}");

  androidAar =
    let
      aar = sdkPackage "android-aar";
    in
    assert lib.assertMsg (
      aar.minimumSdk == lib.toInt minimumSdkVersion
    ) "The libp2p-vpn Android AAR must use API ${minimumSdkVersion}";
    assert lib.assertMsg (
      lib.sort builtins.lessThan aar.abis == lib.sort builtins.lessThan abis
    ) "The libp2p-vpn Android AAR must contain exactly ${lib.concatStringsSep ", " abis}";
    aar;
  androidAarFile = "${androidAar}/${androidAar.aarName}";
  androidNative = androidAar.androidNative;
  androidNativeArm64 = sdkPackage "android-native-arm64";
  androidNativeX86_64 = sdkPackage "android-native-x86_64";
  androidRustTests = sdkPackage "android-rust-tests";

  androidDebugApk =
    if androidProjectPresent then
      androidPkgs.stdenvNoCC.mkDerivation (finalAttrs: {
        inherit pname version;
        src = androidProject;
        strictDeps = true;

        nativeBuildInputs = [
          gradle
          jdk
        ];

        mitmCache = gradle.fetchDeps {
          inherit pname;
          pkg = finalAttrs.finalPackage;
          data = ./android-gradle-deps.json;
        };

        env = {
          ANDROID_HOME = androidHome;
          ANDROID_NDK_ROOT = androidNdkRoot;
          ANDROID_SDK_ROOT = androidHome;
          JAVA_HOME = jdk.home;
          LANG = "C.UTF-8";
          LC_ALL = "C.UTF-8";
          P2P_VPN_ANDROID_AAR = androidAarFile;
        };

        postPatch = ''
          printf 'sdk.dir=%s\n' "$ANDROID_HOME" > local.properties
          export HOME="$TMPDIR/home"
          export ANDROID_USER_HOME="$TMPDIR/android-user-home"
          mkdir -p "$HOME" "$ANDROID_USER_HOME"
          gradleFlagsArray+=(--max-workers "$NIX_BUILD_CORES")
          export JAVA_TOOL_OPTIONS="-XX:ActiveProcessorCount=$NIX_BUILD_CORES"
        '';

        gradleBuildTask = ":app:assembleDebug";
        gradleCheckTask = ":app:testDebugUnitTest :app:lintDebug";
        gradleUpdateTask = ":app:assembleDebug :app:testDebugUnitTest :app:lintDebug";
        gradleFlags = [
          "-Dorg.gradle.project.android.aapt2FromMavenOverride=${androidHome}/build-tools/${buildToolsVersion}/aapt2"
          "-Dorg.gradle.project.android.sync.suppressAgpWarnings=UNSUPPORTED_PROJECT_OPTION_USE"
        ];
        doCheck = true;

        installPhase = ''
          runHook preInstall

          mapfile -t apks < <(find app/build/outputs/apk/debug -type f -name '*.apk' -print)
          if [[ "''${#apks[@]}" -ne 1 ]]; then
            echo "expected one debug APK, found ''${#apks[@]}" >&2
            printf '  %s\n' "''${apks[@]}" >&2
            exit 1
          fi

          install -Dm444 "''${apks[0]}" "$out/p2p-vpn-debug.apk"

          runHook postInstall
        '';

        passthru = {
          available = true;
          apkName = "p2p-vpn-debug.apk";
          abi = arm64Abi;
          inherit abis androidAar androidNative;
          minimumSdk = lib.toInt minimumSdkVersion;
          updateScript = finalAttrs.mitmCache.updateScript;
        };
      })
    else
      unavailable "${pname}-project-missing" (
        "The Android Gradle project does not exist at ${toString androidProject}; "
        + "add android/ before building the debug APK."
      );

  androidEmulatorLauncher =
    if androidProjectPresent then
      androidPkgs.androidenv.emulateApp {
        name = "${pname}-emulator";
        platformVersion = emulatorPlatformVersion;
        abiVersion = x86_64Abi;
        systemImageType = "default";
        app = "${androidDebugApk}/p2p-vpn-debug.apk";
        package = "org.hermeticfoundation.p2pvpn.debug";
        activity = "org.hermeticfoundation.p2pvpn.MainActivity";
        deviceName = "p2p-vpn";
        androidEmulatorFlags = "-no-window -no-audio -no-snapshot -gpu swiftshader_indirect -netdelay none -netspeed full";
        configOptions = {
          "disk.dataPartition.size" = "2G";
          "hw.keyboard" = "yes";
          "hw.ramSize" = "2048";
        };
      }
    else
      unavailable "${pname}-emulator-project-missing" (
        "The Android Gradle project does not exist at ${toString androidProject}; "
        + "add android/ before running the emulator."
      );

  androidEmulator =
    if androidProjectPresent then
      pkgs.writeShellApplication {
        name = "run-test-emulator";
        runtimeInputs = [ pkgs.coreutils ];
        text = ''
          export TMPDIR="''${TMPDIR:-/tmp}"
          export NIX_ANDROID_EMULATOR_FLAGS="''${NIX_ANDROID_EMULATOR_FLAGS:-}"
          export NIX_ANDROID_AVD_FLAGS="''${NIX_ANDROID_AVD_FLAGS:-}"

          emulator_pid=""
          cleanup() {
            local serial="''${ANDROID_SERIAL:-}"
            local android_home="''${ANDROID_HOME:-}"
            local user_home="''${ANDROID_USER_HOME:-}"

            if [[ -n "$serial" && -n "$android_home" ]]; then
              "$android_home/platform-tools/adb" -s "$serial" emu kill >/dev/null 2>&1 || true
            fi
            if [[ -n "$emulator_pid" ]]; then
              for _ in $(seq 1 15); do
                if ! kill -0 "$emulator_pid" 2>/dev/null; then
                  break
                fi
                sleep 1
              done
              if kill -0 "$emulator_pid" 2>/dev/null; then
                kill "$emulator_pid" 2>/dev/null || true
              fi
              wait "$emulator_pid" 2>/dev/null || true
            fi
            case "$user_home" in
              "$TMPDIR"/nix-android-user-home-*)
                chmod -R u+w "$user_home" 2>/dev/null || true
                rm -rf -- "$user_home"
                ;;
            esac
          }

          trap cleanup EXIT
          trap 'exit 130' INT
          trap 'exit 143' TERM

          launcher=${androidEmulatorLauncher}/bin/run-test-emulator
          set +o pipefail
          # shellcheck disable=SC1090
          source "$launcher"
          set -o pipefail
          emulator_pid=$!

          ready_file="''${P2P_VPN_ANDROID_EMULATOR_READY_FILE:-}"
          if [[ -n "$ready_file" ]]; then
            mkdir -p "$(dirname "$ready_file")"
            ready_file_tmp="$ready_file.tmp.$$"
            printf '%s\n' "$ANDROID_SERIAL" > "$ready_file_tmp"
            mv -f "$ready_file_tmp" "$ready_file"
          fi

          printf 'Emulator ready: %s\n' "$ANDROID_SERIAL" >&2
          printf 'Press Ctrl-C to stop it and remove temporary state.\n' >&2
          while kill -0 "$emulator_pid" 2>/dev/null; do
            sleep 2
          done
          wait "$emulator_pid"
        '';
      }
    else
      androidEmulatorLauncher;

  androidDevShell = androidPkgs.mkShellNoCC {
    packages = [
      androidSdk
      gradle
      jdk
      platformTools
    ];

    ANDROID_HOME = androidHome;
    ANDROID_NDK_ROOT = androidNdkRoot;
    ANDROID_SDK_ROOT = androidHome;
    JAVA_HOME = jdk.home;
    LANG = "C.UTF-8";
    LC_ALL = "C.UTF-8";
    P2P_VPN_ANDROID_AAR = androidAarFile;

    shellHook = ''
      export GRADLE_OPTS="''${GRADLE_OPTS:-} -Dorg.gradle.java.home=$JAVA_HOME"
    '';
  };

  androidCheck =
    pkgs.runCommand "${pname}-check"
      {
        nativeBuildInputs = [
          jdk
          pkgs.binutils
          pkgs.unzip
        ];
      }
      ''
        test -e ${androidRustTests}

        emulator_script=${androidEmulator}/bin/run-test-emulator
        if ! grep -F 'while kill -0 "$emulator_pid"' "$emulator_script" >/dev/null \
          || grep -F 'while "$ANDROID_HOME/platform-tools/adb"' "$emulator_script" >/dev/null
        then
          echo "Android emulator ownership must survive transient ADB disconnects" >&2
          exit 1
        fi

        check_load_alignment() {
          local native="$1"
          local abi="$2"
          local alignment
          local load_segments=0

          while read -r alignment; do
            load_segments=$((load_segments + 1))
            if [[ ! "$alignment" =~ ^0x[0-9A-Fa-f]+$ ]] \
              || ((alignment < 0x4000))
            then
              echo "$abi JNI library has unsupported LOAD alignment: $alignment" >&2
              exit 1
            fi
          done < <(readelf -lW "$native" | awk '$1 == "LOAD" { print $NF }')

          if ((load_segments == 0)); then
            echo "$abi JNI library has no ELF LOAD segments" >&2
            exit 1
          fi
        }

        unzip -q ${lib.escapeShellArg androidAarFile} 'jni/*' -d sdk-aar

        arm64_native=sdk-aar/jni/${arm64Abi}/libp2p_vpn_android.so
        if [[ ! -s "$arm64_native" ]]; then
          echo "missing Android JNI library: $arm64_native" >&2
          exit 1
        fi

        arm64_description="$(${pkgs.file}/bin/file -Lb "$arm64_native")"
        case "$arm64_description" in
          *"ARM aarch64"*"for Android ${minimumSdkVersion}"*"built by NDK r27"*) ;;
          *)
            echo "unexpected Android JNI ABI: $arm64_description" >&2
            exit 1
            ;;
        esac
        check_load_alignment "$arm64_native" ${lib.escapeShellArg arm64Abi}

        x86_64_native=sdk-aar/jni/${x86_64Abi}/libp2p_vpn_android.so
        if [[ ! -s "$x86_64_native" ]]; then
          echo "missing Android JNI library: $x86_64_native" >&2
          exit 1
        fi

        x86_64_description="$(${pkgs.file}/bin/file -Lb "$x86_64_native")"
        case "$x86_64_description" in
          *"x86-64"*"for Android ${minimumSdkVersion}"*"built by NDK r28"*) ;;
          *)
            echo "unexpected Android JNI ABI: $x86_64_description" >&2
            exit 1
            ;;
        esac
        check_load_alignment "$x86_64_native" ${lib.escapeShellArg x86_64Abi}

        ${lib.optionalString androidProjectPresent ''
            apk=${androidDebugApk}/p2p-vpn-debug.apk
            if [[ ! -s "$apk" ]]; then
              echo "missing Android debug APK: $apk" >&2
              exit 1
            fi

            apk_files="$(${androidSdk}/bin/apkanalyzer files list "$apk")"
            for abi in ${lib.escapeShellArgs abis}; do
              if ! grep -F "/lib/$abi/libp2p_vpn_android.so" <<<"$apk_files" >/dev/null; then
                echo "APK is missing /lib/$abi/libp2p_vpn_android.so" >&2
                exit 1
              fi
            done

            apk_application_id="$(${androidSdk}/bin/apkanalyzer manifest application-id "$apk")"
            if [[ "$apk_application_id" != org.hermeticfoundation.p2pvpn.debug ]]; then
              echo "APK application ID is $apk_application_id, expected debug-only ID" >&2
              exit 1
            fi

            if ! ${androidHome}/build-tools/${buildToolsVersion}/apksigner verify "$apk"; then
              echo "APK signature verification failed: $apk" >&2
              exit 1
            fi
            if ! ${androidHome}/build-tools/${buildToolsVersion}/zipalign \
              -c -P 16 4 "$apk"
            then
              echo "APK native libraries are not 16 KiB zip-aligned: $apk" >&2
              exit 1
            fi

          apk_minimum_sdk="$(${androidSdk}/bin/apkanalyzer manifest min-sdk "$apk")"
          if [[ "$apk_minimum_sdk" != ${lib.escapeShellArg minimumSdkVersion} ]]; then
            echo "APK declares minSdk $apk_minimum_sdk, expected ${minimumSdkVersion}" >&2
            exit 1
          fi

          apk_target_sdk="$(${androidSdk}/bin/apkanalyzer manifest target-sdk "$apk")"
          if [[ "$apk_target_sdk" != ${lib.escapeShellArg platformVersion} ]]; then
            echo "APK declares targetSdk $apk_target_sdk, expected ${platformVersion}" >&2
            exit 1
          fi

          ${androidHome}/build-tools/${buildToolsVersion}/aapt2 \
            dump resources "$apk" > apk-resources.txt
          for resource in \
            danger \
            muted \
            on_danger \
            on_primary \
            on_secondary \
            on_surface \
            primary \
            secondary \
            surface
          do
            if ! awk -v name="color/$resource" '
              $1 == "resource" { selected = ($3 == name); next }
              selected && $1 == "(night)" { found = 1 }
              END { exit found ? 0 : 1 }
            ' apk-resources.txt
            then
              echo "APK color/$resource is missing a night-qualified value" >&2
              exit 1
            fi
          done
          if ! awk '
            $1 == "resource" { selected = ($3 == "style/Theme.P2pVpn"); next }
            selected && $1 == "(night)" && $2 == "(style)" { found = 1 }
            END { exit found ? 0 : 1 }
          ' apk-resources.txt
          then
            echo "APK Theme.P2pVpn is missing a night-qualified style" >&2
            exit 1
          fi

          ${androidSdk}/bin/apkanalyzer manifest print "$apk" > apk-manifest.xml
          awk '
            /<service/ {
              service = $0 ORS
              in_service = 1
              next
            }
            in_service { service = service $0 ORS }
            in_service && /<\/service>/ {
              if (service ~ /android:name="org\.hermeticfoundation\.p2pvpn\.P2pVpnService"/) {
                printf "%s", service
              }
              service = ""
              in_service = 0
            }
          ' apk-manifest.xml > apk-vpn-service.xml
          if [[ ! -s apk-vpn-service.xml ]] \
            || ! grep -F 'android:exported="true"' apk-vpn-service.xml >/dev/null \
            || ! grep -F 'android:permission="android.permission.BIND_VPN_SERVICE"' \
              apk-vpn-service.xml >/dev/null \
            || ! grep -F 'android:name="android.net.VpnService"' \
              apk-vpn-service.xml >/dev/null \
            || ! grep -F 'android:name="android.net.action.VPN_MANAGER_EVENT"' \
              apk-vpn-service.xml >/dev/null \
            || ! grep -F 'android:name="android.net.category.EVENT_ALWAYS_ON_STATE_CHANGED"' \
              apk-vpn-service.xml >/dev/null
          then
            echo "APK VPN service is missing its protected lifecycle-event contract" >&2
            exit 1
          fi

          if ! grep -F 'android.permission.ACCESS_LOCAL_NETWORK' \
            apk-manifest.xml >/dev/null
          then
            echo "APK must request Android local-network access" >&2
            exit 1
          fi

          if ! grep -A1 -F 'android:name="android.net.VpnService.SUPPORTS_ALWAYS_ON"' \
            apk-manifest.xml | grep -F 'android:value="true"' >/dev/null
          then
            echo "APK must explicitly support always-on VPN mode" >&2
            exit 1
          fi

          if ! grep -F 'org.hermeticfoundation.p2pvpn.DebugAutomationReceiver' \
            apk-manifest.xml >/dev/null \
            || ! grep -F 'android:permission="android.permission.DUMP"' \
              apk-manifest.xml >/dev/null \
            || ! grep -F 'org.hermeticfoundation.p2pvpn.debug.AUTOMATION' \
              apk-manifest.xml >/dev/null
          then
            echo "debug APK is missing the ADB-protected automation receiver" >&2
            exit 1
          fi
        ''}

        mkdir -p "$out"
        printf '%s\n' "$arm64_description" "$x86_64_description" > "$out/native-abi.txt"
        printf '%s\n' ${
          lib.escapeShellArg (
            if androidProjectPresent then "native-and-apk" else "native-only; android/ is not present"
          )
        } > "$out/scope.txt"
      '';

  androidInstall = pkgs.writeShellApplication {
    name = "p2p-vpn-android-install";
    runtimeInputs = lib.optional androidProjectPresent platformTools;
    text =
      if androidProjectPresent then
        ''
          adb_args=()
          if [[ -n "''${ANDROID_SERIAL:-}" ]]; then
            adb_args+=("-s" "$ANDROID_SERIAL")
          fi

          exec adb "''${adb_args[@]}" install -r "$@" \
            ${androidDebugApk}/p2p-vpn-debug.apk
        ''
      else
        ''
          echo "android/ is not present; no debug APK can be installed" >&2
          exit 1
        '';
  };

  androidUpdateDeps = pkgs.writeShellApplication {
    name = "p2p-vpn-android-update-deps";
    text =
      if androidProjectPresent then
        ''
          exec ${androidDebugApk.updateScript} "$@"
        ''
      else
        ''
          echo "android/ is not present; Gradle dependencies cannot be resolved" >&2
          exit 1
        '';
  };
in
{
  inherit
    androidAar
    androidCheck
    androidDebugApk
    androidDevShell
    androidEmulator
    androidInstall
    androidNative
    androidNativeArm64
    androidNativeX86_64
    androidRustTests
    androidSdk
    androidUpdateDeps
    ;

  androidBuildConfig = {
    abi = arm64Abi;
    rustTarget = androidNativeArm64.rustTarget;
    inherit
      abis
      buildToolsVersion
      emulatorPlatformVersion
      ndkVersion
      platformVersion
      ;
    rustTargets = {
      arm64 = androidNativeArm64.rustTarget;
      x86_64 = androidNativeX86_64.rustTarget;
    };
    minimumSdk = lib.toInt minimumSdkVersion;
    inherit androidProjectPresent;
    nativeSourcesPresent = androidAar.available;
    inherit demandLifecycleTrial;
    gradleVersion = gradle.version;
    gradleJdkVersion = lib.versions.major gradle.jdk.version;
    jdkVersion = lib.versions.major jdk.version;
    rustToolchainNdkMajor = androidNativeArm64.toolchainNdkMajor;
  };
}
