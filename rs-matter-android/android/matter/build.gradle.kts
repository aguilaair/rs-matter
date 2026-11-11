plugins {
    id("com.android.library")
}

// The Rust half (librs_matter_android.so) is built by ../build-rust.sh into src/main/jniLibs/<abi>/.
// -PrustAbis=armeabi-v7a,arm64-v8a picks the ABIs (default: all four Android ABIs).
val rustAbis: List<String> = ((findProperty("rustAbis") as String?) ?: "armeabi-v7a,arm64-v8a,x86_64,x86")
    .split(',').map { it.trim() }.filter { it.isNotEmpty() }

// The directory with build-rust.sh: this module's parent, also when another project includes it.
val androidDir: File = projectDir.parentFile

val buildRust by tasks.registering(Exec::class) {
    group = "build"
    description = "Builds librs_matter_android.so for $rustAbis with cargo and the NDK"
    workingDir = androidDir
    commandLine(listOf(androidDir.resolve("build-rust.sh").absolutePath) + rustAbis)
    outputs.dir(layout.projectDirectory.dir("src/main/jniLibs"))
    // The library depends on the whole rs-matter workspace; cargo knows what changed (and is quick
    // when nothing did), Gradle doesn't.
    outputs.upToDateWhen { false }
}

android {
    namespace = "org.rsmatter.android"
    compileSdk = 36

    defaultConfig {
        minSdk = 26
        consumerProguardFiles("consumer-rules.pro")
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

tasks.named("preBuild") { dependsOn(buildRust) }

dependencies {
    implementation("androidx.annotation:annotation:1.9.1")
}
