{
  pkgs,
  src ? ../.,
  version ? "0.1.0",
  androidSdkLicenseAccepted ? false,
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
  minimumSdkVersion = "26";
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

  androidCrossSystem = lib.systems.examples.aarch64-android-prebuilt // {
    androidSdkVersion = minimumSdkVersion;
  };
  androidCrossPkgs = import pkgs.path {
    localSystem = pkgs.stdenv.hostPlatform.system;
    crossSystem = androidCrossSystem;
    config = androidNixpkgsConfig;
  };
  arm64RustTarget = androidCrossPkgs.stdenv.hostPlatform.rust.rustcTarget;
  x86_64RustTarget = "x86_64-linux-android";

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
  rootManifest = src + "/Cargo.toml";
  rootLock = src + "/Cargo.lock";
  rootRustSource = src + "/src";
  androidCrate = src + "/crates/p2p-vpn-android";
  androidE2eFixtureCrate = src + "/crates/p2p-vpn-android-e2e-fixture";
  androidProjectPresent = builtins.pathExists androidProject;
  nativeSourcesPresent = builtins.all builtins.pathExists [
    rootManifest
    rootLock
    rootRustSource
    androidCrate
    androidE2eFixtureCrate
  ];

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

  nativeSource = lib.fileset.toSource {
    root = src;
    fileset = lib.fileset.unions [
      rootManifest
      rootLock
      rootRustSource
      androidCrate
      androidE2eFixtureCrate
    ];
  };

  androidNativeArm64 =
    if nativeSourcesPresent then
      assert lib.assertMsg (
        androidCrossPkgs.stdenv.hostPlatform.androidSdkVersion == minimumSdkVersion
      ) "p2p-vpn Android Rust cross target must use API ${minimumSdkVersion}";
      assert lib.assertMsg (
        androidCrossPkgs.stdenv.hostPlatform.androidNdkVersion == "27"
      ) "p2p-vpn Android Rust cross target must retain the nixpkgs prebuilt NDK 27 toolchain";
      assert lib.assertMsg (androidCrossPkgs.stdenv.hostPlatform.useAndroidPrebuilt or false
      ) "p2p-vpn Android Rust cross target must retain the nixpkgs prebuilt Android toolchain";
      androidCrossPkgs.rustPlatform.buildRustPackage {
        pname = "${pname}-arm64";
        inherit version;
        src = nativeSource;
        cargoLock.lockFile = rootLock;
        cargoBuildFlags = [
          "--package"
          "p2p-vpn-android"
          "--lib"
        ];
        doCheck = false;
        strictDeps = true;

        installPhase = ''
          runHook preInstall

          shared_library="target/${arm64RustTarget}/release/libp2p_vpn_android.so"
          if [[ ! -s "$shared_library" ]]; then
            echo "missing Android JNI library: $shared_library" >&2
            exit 1
          fi

          install -Dm755 \
            "$shared_library" \
            "$out/lib/${arm64Abi}/libp2p_vpn_android.so"

          runHook postInstall
        '';

        passthru = {
          available = true;
          abi = arm64Abi;
          rustTarget = arm64RustTarget;
          minimumSdk = lib.toInt minimumSdkVersion;
          toolchainNdkMajor = 27;
        };
      }
    else
      unavailable "${pname}-native-source-missing" (
        "The Rust Android source is incomplete under ${toString src}; "
        + "Cargo.toml, Cargo.lock, src/, and crates/p2p-vpn-android/ are required."
      );

  # Nixpkgs only provides a prebuilt Rust standard library for Android arm64.
  # Build std for x86_64 with the pinned NDK so the APK can run in a KVM emulator.
  x86_64RustcSysroot = pkgs.symlinkJoin {
    name = "rustc-with-android-libsrc";
    paths = [ pkgs.rustc.unwrapped ];
    postBuild = ''
      mkdir -p "$out/lib/rustlib/src/rust"
      ln -s ${pkgs.rustPlatform.rustLibSrc} "$out/lib/rustlib/src/rust/library"
    '';
  };
  x86_64Rustc = pkgs.rustc.override { sysroot = x86_64RustcSysroot; };
  x86_64RustPlatform = pkgs.makeRustPlatform {
    cargo = pkgs.cargo;
    rustc = x86_64Rustc;
  };
  x86_64BaseCargoDeps = x86_64RustPlatform.importCargoLock { lockFile = rootLock; };
  x86_64CargoDeps = pkgs.symlinkJoin {
    # importCargoLock's config resolves this relative directory name.
    name = "cargo-vendor-dir";
    paths = [ x86_64BaseCargoDeps ];
    postBuild = ''
      for crate in ${pkgs.rustPlatform.rustLibSrc}/vendor/*; do
        name="$(basename "$crate")"
        if [[ ! -e "$out/$name" ]]; then
          ln -s "$crate" "$out/$name"
        fi
      done
    '';
  };
  x86_64Linker = "${androidNdkRoot}/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android${minimumSdkVersion}-clang";

  androidNativeX86_64 =
    if nativeSourcesPresent then
      x86_64RustPlatform.buildRustPackage {
        pname = "${pname}-x86_64";
        inherit version;
        src = nativeSource;
        cargoDeps = x86_64CargoDeps;
        doCheck = false;
        strictDeps = true;
        auditable = false;

        env = {
          RUSTC_BOOTSTRAP = "1";
          CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER = x86_64Linker;
          CC_x86_64_linux_android = x86_64Linker;
        };

        buildPhase = ''
          runHook preBuild

          cargo build \
            -Z build-std=std,panic_abort \
            --target ${x86_64RustTarget} \
            --offline \
            --release \
            --package p2p-vpn-android \
            --lib

          runHook postBuild
        '';

        installPhase = ''
          runHook preInstall

          shared_library="target/${x86_64RustTarget}/release/libp2p_vpn_android.so"
          if [[ ! -s "$shared_library" ]]; then
            echo "missing Android JNI library: $shared_library" >&2
            exit 1
          fi

          install -Dm755 \
            "$shared_library" \
            "$out/lib/${x86_64Abi}/libp2p_vpn_android.so"

          runHook postInstall
        '';

        passthru = {
          available = true;
          abi = x86_64Abi;
          rustTarget = x86_64RustTarget;
          minimumSdk = lib.toInt minimumSdkVersion;
          toolchainNdkMajor = 28;
        };
      }
    else
      unavailable "${pname}-x86_64-native-source-missing" (
        "The Rust Android source is incomplete under ${toString src}; "
        + "Cargo.toml, Cargo.lock, src/, and crates/p2p-vpn-android/ are required."
      );

  androidNative = pkgs.symlinkJoin {
    name = "${pname}-native-${version}";
    paths = [
      androidNativeArm64
      androidNativeX86_64
    ];
    passthru = {
      available = nativeSourcesPresent;
      inherit abis androidNativeArm64 androidNativeX86_64;
      minimumSdk = lib.toInt minimumSdkVersion;
    };
  };

  androidRustTests =
    if nativeSourcesPresent then
      pkgs.rustPlatform.buildRustPackage {
        pname = "${pname}-rust-tests";
        inherit version;
        src = nativeSource;
        cargoLock.lockFile = rootLock;
        cargoBuildFlags = [
          "--package"
          "p2p-vpn-android"
          "--tests"
        ];
        cargoTestFlags = [
          "--package"
          "p2p-vpn-android"
        ];
        doCheck = true;
        strictDeps = true;

        installPhase = ''
          runHook preInstall
          touch "$out"
          runHook postInstall
        '';
      }
    else
      unavailable "${pname}-rust-tests-source-missing" (
        "The Rust Android source is incomplete under ${toString src}; "
        + "Cargo.toml, Cargo.lock, src/, and crates/p2p-vpn-android/ are required."
      );

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
        };

        postPatch = ''
          for abi in ${lib.escapeShellArgs abis}; do
            mkdir -p "app/src/main/jniLibs/$abi"
            rm -f "app/src/main/jniLibs/$abi/libp2p_vpn_android.so"
            install -m755 \
              "${androidNative}/lib/$abi/libp2p_vpn_android.so" \
              "app/src/main/jniLibs/$abi/libp2p_vpn_android.so"
          done

          printf 'sdk.dir=%s\n' "$ANDROID_HOME" > local.properties
          export HOME="$TMPDIR/home"
          export ANDROID_USER_HOME="$TMPDIR/android-user-home"
          mkdir -p "$HOME" "$ANDROID_USER_HOME"
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
          inherit abis androidNative;
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
          while "$ANDROID_HOME/platform-tools/adb" -s "$ANDROID_SERIAL" get-state >/dev/null 2>&1; do
            sleep 2
          done
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

    shellHook = ''
      export GRADLE_OPTS="''${GRADLE_OPTS:-} -Dorg.gradle.java.home=$JAVA_HOME"
    '';
  };

  androidCheck =
    pkgs.runCommand "${pname}-check"
      {
        nativeBuildInputs = [ jdk ];
      }
      ''
        test -e ${androidRustTests}

        arm64_native=${androidNative}/lib/${arm64Abi}/libp2p_vpn_android.so
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

        x86_64_native=${androidNative}/lib/${x86_64Abi}/libp2p_vpn_android.so
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

          ${androidSdk}/bin/apkanalyzer manifest print "$apk" > apk-manifest.xml
          if ! grep -A1 -F 'android:name="android.net.VpnService.SUPPORTS_ALWAYS_ON"' \
            apk-manifest.xml | grep -F 'android:value="false"' >/dev/null
          then
            echo "APK must explicitly opt out of unsupported always-on VPN mode" >&2
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
    rustTarget = arm64RustTarget;
    inherit
      abis
      buildToolsVersion
      emulatorPlatformVersion
      ndkVersion
      platformVersion
      ;
    rustTargets = {
      arm64 = arm64RustTarget;
      x86_64 = x86_64RustTarget;
    };
    minimumSdk = lib.toInt minimumSdkVersion;
    inherit androidProjectPresent nativeSourcesPresent;
    gradleVersion = gradle.version;
    gradleJdkVersion = lib.versions.major gradle.jdk.version;
    jdkVersion = lib.versions.major jdk.version;
    rustToolchainNdkMajor = lib.toInt androidCrossPkgs.stdenv.hostPlatform.androidNdkVersion;
    rustUsesAndroidPrebuilt = androidCrossPkgs.stdenv.hostPlatform.useAndroidPrebuilt or false;
  };
}
