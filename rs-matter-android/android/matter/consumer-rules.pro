# The Rust side calls these by name through JNI.
-keep class org.rsmatter.android.MatterNode {
    native <methods>;
    *** ble*(...);
    *** on*(...);
}
