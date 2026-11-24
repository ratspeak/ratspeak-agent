use std::process::Command;

pub(crate) fn curl() -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut command = Command::new("curl.exe");
        command.creation_flags(0x0800_0000);
        command
    }
    #[cfg(not(windows))]
    Command::new("curl")
}
