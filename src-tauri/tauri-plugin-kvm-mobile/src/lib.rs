use tauri::{
    Runtime,
    plugin::{Builder, TauriPlugin},
};

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("kvm-mobile")
        .setup(|_app, api| {
            #[cfg(target_os = "android")]
            api.register_android_plugin("local.kvm.mobile", "KvmPlugin")?;
            Ok(())
        })
        .build()
}
