{
  pkgs,
  src ? ../.,
  version ? "0.1.0",
  androidSdkLicenseAccepted ? false,
}:

let
  inherit (pkgs) lib;

  pname = "p2p-vpn-android";
  abi = "arm64-v8a";
  minimumSdkVersion = "26";
  platformVersion = "37";
  buildToolsVersion = "37.0.0";
  ndkVersion = "28.2.13676358";

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
  rustTarget = androidCrossPkgs.stdenv.hostPlatform.rust.rustcTarget;

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
  androidProjectPresent = builtins.pathExists androidProject;
  nativeSourcesPresent = builtins.all builtins.pathExists [
    rootManifest
    rootLock
    rootRustSource
    androidCrate
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
    ];
  };

  androidNative =
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
        inherit pname version;
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

          shared_library="target/${rustTarget}/release/libp2p_vpn_android.so"
          if [[ ! -s "$shared_library" ]]; then
            echo "missing Android JNI library: $shared_library" >&2
            exit 1
          fi

          install -Dm755 \
            "$shared_library" \
            "$out/lib/${abi}/libp2p_vpn_android.so"

          runHook postInstall
        '';

        passthru = {
          available = true;
          inherit abi rustTarget;
          minimumSdk = lib.toInt minimumSdkVersion;
          toolchainNdkMajor = 27;
        };
      }
    else
      unavailable "${pname}-native-source-missing" (
        "The Rust Android source is incomplete under ${toString src}; "
        + "Cargo.toml, Cargo.lock, src/, and crates/p2p-vpn-android/ are required."
      );

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
          mkdir -p app/src/main/jniLibs/${abi}
          rm -f app/src/main/jniLibs/${abi}/libp2p_vpn_android.so
          install -m755 \
            ${androidNative}/lib/${abi}/libp2p_vpn_android.so \
            app/src/main/jniLibs/${abi}/libp2p_vpn_android.so

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
          inherit abi androidNative;
          minimumSdk = lib.toInt minimumSdkVersion;
          updateScript = finalAttrs.mitmCache.updateScript;
        };
      })
    else
      unavailable "${pname}-project-missing" (
        "The Android Gradle project does not exist at ${toString androidProject}; "
        + "add android/ before building the debug APK."
      );

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

        native=${androidNative}/lib/${abi}/libp2p_vpn_android.so
        if [[ ! -s "$native" ]]; then
          echo "missing Android JNI library: $native" >&2
          exit 1
        fi

        native_description="$(${pkgs.file}/bin/file -b "$native")"
        case "$native_description" in
          *"ARM aarch64"*"for Android ${minimumSdkVersion}"*"built by NDK r27"*) ;;
          *)
            echo "unexpected Android JNI ABI: $native_description" >&2
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
            if ! grep -F '/lib/${abi}/libp2p_vpn_android.so' <<<"$apk_files" >/dev/null; then
              echo "APK is missing /lib/${abi}/libp2p_vpn_android.so" >&2
              exit 1
            fi

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
        ''}

        mkdir -p "$out"
        printf '%s\n' "$native_description" > "$out/native-abi.txt"
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
    androidInstall
    androidNative
    androidRustTests
    androidSdk
    androidUpdateDeps
    ;

  androidBuildConfig = {
    inherit
      abi
      buildToolsVersion
      ndkVersion
      platformVersion
      rustTarget
      ;
    minimumSdk = lib.toInt minimumSdkVersion;
    inherit androidProjectPresent nativeSourcesPresent;
    gradleVersion = gradle.version;
    gradleJdkVersion = lib.versions.major gradle.jdk.version;
    jdkVersion = lib.versions.major jdk.version;
    rustToolchainNdkMajor = lib.toInt androidCrossPkgs.stdenv.hostPlatform.androidNdkVersion;
    rustUsesAndroidPrebuilt = androidCrossPkgs.stdenv.hostPlatform.useAndroidPrebuilt or false;
  };
}
