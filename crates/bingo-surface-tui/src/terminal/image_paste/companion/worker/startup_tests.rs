//! A real owner process dies before its helper can report readiness.

use super::*;

#[test]
#[ignore = "subprocess fixture; invoked by owner_death_during_startup_kills_the_job"]
fn startup_owner_fixture() {
    let path = std::env::var("BINGO_IMAGE_PASTE_STARTUP_TEST_FILE").unwrap();
    let path = path.replace('\'', "''");
    let command = super::tests::fake_helper(&format!(
        "[IO.File]::WriteAllText('{path}', [string]$PID); Start-Sleep -Seconds 120"
    ));
    let _ = Worker::start(command);
}

#[test]
fn owner_death_during_startup_kills_the_job() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("helper-pid");
    let file = file.to_string_lossy().replace('\'', "''");
    let executable = std::env::current_exe().unwrap();
    let executable = executable.to_string_lossy().replace('\'', "''");
    let module = module_path!().split_once("::").unwrap().1;
    let test = format!("{module}::startup_owner_fixture");
    let script = format!(
        "$ErrorActionPreference='Stop'; \
         $i=New-Object Diagnostics.ProcessStartInfo; $i.FileName='{executable}'; \
         $i.Arguments='--exact {test} --ignored --nocapture'; \
         $i.UseShellExecute=$false; $i.CreateNoWindow=$true; \
         $i.EnvironmentVariables['BINGO_IMAGE_PASTE_STARTUP_TEST_FILE']='{file}'; \
         $o=[Diagnostics.Process]::Start($i); $h=$null; \
         try {{ \
           $end=[DateTime]::UtcNow.AddSeconds(15); \
           while (!(Test-Path -LiteralPath '{file}')) {{ \
             if ($o.HasExited -or [DateTime]::UtcNow -ge $end) {{ throw 'startup fixture unavailable' }}; \
             Start-Sleep -Milliseconds 20; \
           }}; \
           $h=[Diagnostics.Process]::GetProcessById([int][IO.File]::ReadAllText('{file}')); \
           if ($h.HasExited) {{ throw 'helper exited before owner death' }}; \
           $o.Kill(); $o.WaitForExit(); \
           if (!$h.WaitForExit(5000)) {{ throw 'pre-readiness helper survived owner death' }}; \
         }} finally {{ \
           if (!$o.HasExited) {{ $o.Kill(); $o.WaitForExit() }}; \
           if ($h -and !$h.HasExited) {{ $h.Kill(); $h.WaitForExit() }}; \
         }}"
    );
    let output = super::tests::fake_helper(&script).output().unwrap();
    assert!(
        output.status.success(),
        "startup JobObject test failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
