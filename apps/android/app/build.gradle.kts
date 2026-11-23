plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "com.techhouseblr.meshwav"
    compileSdk = 34
    defaultConfig {
        applicationId = "com.techhouseblr.meshwav"
        minSdk = 28 // ChaCha20-Poly1305 in javax.crypto
        targetSdk = 34
        versionCode = 9
        versionName = "0.7.0"
    }
    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.getByName("debug") // sideload build; real signing later
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions { jvmTarget = "17" }
}

dependencies {
    testImplementation("junit:junit:4.13.2")
}
