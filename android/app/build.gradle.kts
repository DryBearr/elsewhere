// The Android app. Before building, the scripts put the Rust core, Python, ffmpeg and
// the Kotlin bindings under ../target/android (see the README), and this build picks
// them up from there:
//   python3 scripts/mobile_python.py android
//   scripts/android_ffmpeg.sh
//   scripts/android_rust.sh
//   gradle -p android assembleRelease
plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

// One version for both apps: `version` in the Rust core's Cargo.toml.
val version = Regex("""(?m)^version = "(\d+)\.(\d+)\.(\d+)"""")
    .find(rootDir.resolve("../Cargo.toml").readText())!!
    .destructured.toList().map { it.toInt() }
val generated = rootDir.resolve("../target/android")

android {
    namespace = "io.github.drybearr.elsewhere"
    compileSdk = 35

    defaultConfig {
        applicationId = "io.github.drybearr.elsewhere"
        minSdk = 26
        targetSdk = 35
        versionName = version.joinToString(".")
        // The numbering cargo-apk used for 0.1 and 0.2, so this app installs over them.
        versionCode = (1 shl 24) or (version[0] shl 16) or (version[1] shl 8) or version[2]
        // The UI tests (src/androidTest): gradle connectedDebugAndroidTest.
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    signingConfigs {
        create("release") {
            // The release workflow passes the key; local release builds stay unsigned.
            System.getenv("ELSEWHERE_KEYSTORE")?.let { keystore ->
                storeFile = file(keystore)
                storeType = "pkcs12"
                storePassword = System.getenv("ELSEWHERE_KEYSTORE_PASSWORD")
                keyAlias = "elsewhere"
                keyPassword = System.getenv("ELSEWHERE_KEYSTORE_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            if (System.getenv("ELSEWHERE_KEYSTORE") != null) {
                signingConfig = signingConfigs.getByName("release")
            }
        }
    }

    sourceSets["main"].apply {
        jniLibs.srcDir(generated.resolve("libs"))
        assets.srcDir(generated.resolve("assets"))
        java.srcDir(generated.resolve("kotlin"))
    }
    // Debug builds carry the player's test files for the self-test (SelfTest.kt).
    sourceSets["debug"].assets.srcDir(rootDir.resolve("../tests/media"))

    // Unpack native libraries onto disk: ffmpeg and ffprobe ship as libffmpeg.so and
    // libffprobe.so and run from there.
    packaging {
        jniLibs.useLegacyPackaging = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures {
        compose = true
    }
}

kotlin {
    jvmToolchain(17)
}

dependencies {
    implementation(platform("androidx.compose:compose-bom:2024.12.01"))
    implementation("androidx.compose.material3:material3")
    implementation("androidx.activity:activity-compose:1.9.3")
    // UniFFI's Kotlin bindings reach the Rust core through JNA.
    implementation("net.java.dev.jna:jna:5.15.0@aar")

    // The UI tests only; none of it goes into the app.
    androidTestImplementation(platform("androidx.compose:compose-bom:2024.12.01"))
    androidTestImplementation("androidx.compose.ui:ui-test-junit4")
    androidTestImplementation("androidx.test:runner:1.6.2")
    androidTestImplementation("androidx.test.ext:junit:1.2.1")
    debugImplementation("androidx.compose.ui:ui-test-manifest")
}
