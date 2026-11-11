plugins {
    id("com.android.application")
}

android {
    namespace = "org.rsmatter.android.sample"
    compileSdk = 36

    defaultConfig {
        applicationId = "org.rsmatter.android.sample"
        minSdk = 26
        targetSdk = 34
        versionCode = 1
        versionName = "0.1.0"
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    implementation(project(":matter"))
}
