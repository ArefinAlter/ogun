// The Android library of the Ogun Studio field gateway plugin. `tauri android init` / `tauri android build`
// includes it in the app's Gradle project together with `tauri-android`.
plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "dev.verne.ogun.gateway"
    // 35 for Service.onTimeout (Android 15's daily limit on dataSync foreground services).
    compileSdk = 35

    defaultConfig {
        // 26: notification channels and Keystore AES-GCM without fallbacks.
        minSdk = 26
        consumerProguardFiles("consumer-rules.pro")
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }
    kotlinOptions {
        jvmTarget = "1.8"
    }
}

dependencies {
    // Only the Tauri plugin API; sockets, TLS, JSON, the Keystore and JobScheduler come from the platform.
    implementation(project(":tauri-android"))
}
