// JVM / Android library build. The SDK has no dependencies beyond the
// Kotlin standard library. Offline checks: `scripts/test.sh`.
plugins {
    kotlin("jvm") version "2.2.21"
    `java-library`
}

group = "com.railyard"
version = "0.1.0"

repositories { mavenCentral() }

kotlin { jvmToolchain(17) }

tasks.register<JavaExec>("selfTest") {
    group = "verification"
    description = "Runs the dependency-free SDK checks."
    classpath = sourceSets["test"].runtimeClasspath
    mainClass.set("com.railyard.analytics.AnalyticsTestKt")
}
tasks.named("check") { dependsOn("selfTest") }
