namespace Bingo.ImagePaste {
    static class TestNative {
        [System.Runtime.InteropServices.DllImport("kernel32.dll")]
        public static extern bool AllocConsole();
        [System.Runtime.InteropServices.DllImport("kernel32.dll")]
        public static extern bool SetConsoleMode(System.IntPtr input, uint mode);
        [System.Runtime.InteropServices.DllImport("kernel32.dll", CharSet = System.Runtime.InteropServices.CharSet.Unicode)]
        public static extern bool ReadConsoleInputW(System.IntPtr input,
            [System.Runtime.InteropServices.Out] Native.InputRecord[] records, uint count, out uint read);
        [System.Runtime.InteropServices.DllImport("kernel32.dll")]
        public static extern bool FlushConsoleInputBuffer(System.IntPtr input);
    }
    public static class ContractTests {
        sealed class Fake : IPasteTarget {
            public bool ChordValue = true, ScopeValue = true, FocusValue = true;
            public bool BitmapValue = true, InjectValue = true, Throw;
            public int ClipboardCalls, InjectCalls, ScopeCalls, FocusCalls;
            public int FailFocusAt, FailScopeAt;
            public bool Chord() { if (Throw) throw new System.Exception(); return ChordValue; }
            public bool Scope() { ScopeCalls++; return ScopeValue && ScopeCalls != FailScopeAt; }
            public bool Focused() { FocusCalls++; return FocusValue && FocusCalls != FailFocusAt; }
            public bool Bitmap() { ClipboardCalls++; return BitmapValue; }
            public bool Inject() { InjectCalls++; return InjectValue; }
        }
        static void Check(bool value, string message) {
            if (!value) throw new System.Exception(message);
        }
        static void Reject(Fake fake, string reason) {
            PastePolicy policy = new PastePolicy(fake);
            Check(!policy.Key(0x100, 0x56, 0), reason);
            Check(fake.InjectCalls == 0, reason + " injected input");
        }
        public static void Policy() {
            Check(Protocol.Ready == "BINGO_IMAGE_PASTE_READY_V1", "readiness protocol");
            Check(Protocol.MarkerValid(42, "[bingo-image-paste-42-abcd-0]"), "bracketed marker");
            Check(!Protocol.MarkerValid(43, "[bingo-image-paste-42-abcd-0]"), "owner-specific marker");
            Check(!Protocol.MarkerValid(42, "[bingo-image-paste-42-abcd-0]\n"), "marker newline");
            Check(!Protocol.MarkerValid(42, "[bingo-image-paste-42-abcd-0]'"), "marker command escape");
            Check(!Protocol.MarkerValid(0, null), "missing owner/marker");
            Check(Target.ChordMatches(true, false, false, false, false), "CtrlV chord");
            Check(!Target.ChordMatches(false, false, false, false, false), "plain V");
            Check(!Target.ChordMatches(true, true, false, false, false), "CtrlShiftV");
            Check(!Target.ChordMatches(true, false, true, false, false), "CtrlAltV");
            Check(!Target.ChordMatches(true, false, false, true, false), "CtrlLeftWinV");
            Check(!Target.ChordMatches(true, false, false, false, true), "CtrlRightWinV");
            Fake fake = new Fake();
            PastePolicy policy = new PastePolicy(fake);
            Check(policy.Key(0x100, 0x56, 0), "physical image CtrlV");
            Check(policy.Key(0x100, 0x56, 0), "repeat suppressed");
            Check(policy.Key(0x101, 0x56, 0), "paired up suppressed");
            Check(fake.InjectCalls == 1, "one injection per chord");
            Check(policy.Key(0x100, 0x56, 0), "next chord allowed");
            Check(fake.InjectCalls == 2, "second chord injected");
            fake = new Fake { ChordValue = false }; Reject(fake, "plain V/modifier combination");
            Check(fake.ClipboardCalls == 0, "nonchord inspected clipboard");
            fake = new Fake { ScopeValue = false }; Reject(fake, "other window/dead owner/nonraw console");
            Check(fake.ClipboardCalls == 0, "out-of-scope inspected clipboard");
            fake = new Fake { FocusValue = false }; Reject(fake, "other pane/uncertain UIA");
            Check(fake.ClipboardCalls == 0, "unfocused inspected clipboard");
            Reject(new Fake { BitmapValue = false }, "text-only clipboard");
            Reject(new Fake { FailFocusAt = 2 }, "focus changes after bitmap gate");
            Reject(new Fake { FailScopeAt = 2 }, "scope changes before injection");
            Reject(new Fake { Throw = true }, "probe exception");
            fake = new Fake { InjectValue = false }; policy = new PastePolicy(fake);
            Check(!policy.Key(0x100, 0x56, 0), "injection failure must pass original");
            Check(!policy.Key(0x101, 0x56, 0), "failure must pass keyup");
            fake = new Fake(); policy = new PastePolicy(fake);
            Check(!policy.Key(0x100, 0x43, 0), "CtrlC unchanged");
            Check(!policy.Key(0x100, 0x56, 0x10), "injected unchanged");
            Check(!policy.Key(0x100, 0x56, 2), "lower-integrity injected unchanged");
            Check(!policy.Key(0x104, 0x56, 0), "system Alt chord unchanged");
            Check(!policy.Key(0x101, 0x56, 0), "unrelated keyup unchanged");
            Check(fake.ClipboardCalls == 0 && fake.InjectCalls == 0, "unrelated keys touched clipboard/input");
            HeldLoss(true); HeldLoss(false);
            PaneIsolation();
            MixedClipboardFormats();
            BoundedFocus();
        }
        static void HeldLoss(bool scopeLoss) {
            Fake fake = new Fake(); PastePolicy policy = new PastePolicy(fake);
            Check(policy.Key(0x100, 0x56, 0), "held precondition");
            if (scopeLoss) fake.ScopeValue = false; else fake.ChordValue = false;
            Check(!policy.Key(0x100, 0x56, 0), "held repeat after scope/modifier loss");
            Check(!policy.Key(0x101, 0x56, 0), "held up after scope/modifier loss");
            Check(fake.InjectCalls == 1, "held loss reinjected");
        }
        static void PaneIsolation() {
            string marker = "[bingo-image-paste-42-abcd-0]";
            Check(FocusProbe.PaneMatches(true, "TermControl", "Bingo - old " + marker, marker), "exact focused pane");
            Check(FocusProbe.PaneMatches(true, "TermControl", "Bingo - updated " + marker, marker), "updated pane title");
            Check(!FocusProbe.PaneMatches(false, "TermControl", "Bingo " + marker, marker), "unfocused pane");
            Check(!FocusProbe.PaneMatches(true, "OtherControl", "Bingo " + marker, marker), "nonterminal focus");
            Check(!FocusProbe.PaneMatches(true, "TermControl", "Bingo [bingo-image-paste-43-abcd-0]", marker), "other owner pane");
            Check(!FocusProbe.PaneMatches(true, "TermControl", "Bingo [bingo-image-paste-42-abcd-1]", marker), "other run pane");
            Check(!FocusProbe.PaneMatches(true, "TermControl", "Bingo " + marker + " suffix", marker), "marker must be suffix");
            Check(!FocusProbe.PaneMatches(true, "TermControl", null, marker), "missing pane metadata");
            Check(!FocusProbe.PaneMatches(true, "TermControl", "Bingo", ""), "missing marker");
        }
        static bool Formats(params uint[] formats) {
            return ClipboardPolicy.ImageOnly(delegate(uint format) { return System.Array.IndexOf(formats, format) >= 0; });
        }
        static void MixedClipboardFormats() {
            Check(!Formats(), "empty clipboard");
            foreach (uint bitmap in new uint[] { 2, 8, 17 }) {
                Check(Formats(bitmap), "image-only format accepted");
                foreach (uint text in new uint[] { 1, 7, 13 }) {
                    Check(!Formats(text), "text-only format preserved");
                    Check(!Formats(bitmap, text), "mixed text and bitmap preserved");
                    Reject(new Fake { BitmapValue = Formats(bitmap, text) }, "mixed clipboard must not inject");
                }
            }
            Check(Formats(2, 8, 17), "multiple bitmap formats");
            Check(!Formats(1, 7, 13, 2, 8, 17), "rich clipboard with every standard format");
        }
        static void BoundedFocus() {
            System.Threading.ManualResetEvent release = new System.Threading.ManualResetEvent(false);
            System.Threading.ManualResetEvent returned = new System.Threading.ManualResetEvent(false);
            int calls = 0;
            using (FocusProbe probe = new FocusProbe("marker", delegate {
                int call = System.Threading.Interlocked.Increment(ref calls);
                release.WaitOne(); returned.Set(); return call == 1;
            })) {
                Check(!probe.Matches(), "hung UIA must time out");
                for (int i = 0; i < 100; i++) Check(!probe.Matches(), "busy UIA must fail open");
                Check(calls == 1, "hung UIA spawned more work");
                release.Set();
                Check(returned.WaitOne(5000), "test worker did not return");
                // The timed-out positive belongs to its expired request, never a key.
                System.Diagnostics.Stopwatch budget = System.Diagnostics.Stopwatch.StartNew();
                while (System.Threading.Volatile.Read(ref calls) < 2 && budget.ElapsedMilliseconds < 5000) {
                    Check(!probe.Matches(), "expired positive result reused");
                    System.Threading.Thread.Yield();
                }
                Check(calls == 2, "fresh focus request did not execute");
            }
        }
        static string CompileCommand(string sourcePath, bool tests) {
            string path = sourcePath.Replace("'", "''");
            string source = "[IO.File]::ReadAllText('" + path + "')";
            if (tests) source += "+[IO.File]::ReadAllText('" + path.Replace(".cs", ".tests.cs") + "')";
            return "$ErrorActionPreference='Stop';$f=[System.Runtime.InteropServices.RuntimeEnvironment]::GetRuntimeDirectory();"
                + "Add-Type -TypeDefinition (" + source + ") -ReferencedAssemblies @('System.dll',"
                + "($f+'WPF/UIAutomationClient.dll'),($f+'WPF/UIAutomationTypes.dll'),($f+'WPF/WindowsBase.dll'));";
        }
        static System.Diagnostics.Process Spawn(string command) {
            System.Diagnostics.ProcessStartInfo info = new System.Diagnostics.ProcessStartInfo();
            info.FileName = System.IO.Path.Combine(System.Environment.GetFolderPath(System.Environment.SpecialFolder.System),
                "WindowsPowerShell\\v1.0\\powershell.exe");
            info.Arguments = "-NoLogo -NoProfile -NonInteractive -Command \"" + command + "\"";
            info.UseShellExecute = false;
            info.CreateNoWindow = true;
            info.RedirectStandardInput = info.RedirectStandardOutput = info.RedirectStandardError = true;
            return System.Diagnostics.Process.Start(info);
        }
        static string Line(System.Diagnostics.Process process) {
            System.Threading.Tasks.Task<string> line = process.StandardOutput.ReadLineAsync();
            Check(line.Wait(15000), "bounded child readiness timed out");
            if (line.Result == null) throw new System.Exception("child exited before ready: " + process.StandardError.ReadToEnd());
            return line.Result;
        }
        static void Stop(System.Diagnostics.Process process) {
            if (process == null) return;
            if (!process.HasExited) { process.Kill(); Check(process.WaitForExit(5000), "child cleanup"); }
            process.Dispose();
        }
        static System.Diagnostics.Process Owner(string sourcePath, bool linger) {
            return Spawn(CompileCommand(sourcePath, true) + "[Bingo.ImagePaste.ContractTests]::OwnerHost("
                + (linger ? "$true" : "$false") + ")");
        }
        static System.Diagnostics.Process Bridge(string sourcePath, uint owner) {
            string marker = "[bingo-image-paste-" + owner + "-abcdef-0]";
            return Spawn(CompileCommand(sourcePath, false) + "exit [Bingo.ImagePaste.Companion]::Start("
                + owner + ",'" + marker + "')");
        }
        public static void OwnerHost(bool linger) {
            System.IO.Stream stdin = System.Console.OpenStandardInput();
            System.IO.StreamWriter stdout = new System.IO.StreamWriter(System.Console.OpenStandardOutput());
            Native.FreeConsole();
            Check(TestNative.AllocConsole(), "owner isolated console");
            Native.SetConsoleCtrlHandler(System.IntPtr.Zero, true);
            System.IntPtr input = Native.OpenInput();
            uint mode;
            Check(Native.GetConsoleMode(input, out mode) && TestNative.SetConsoleMode(input, mode & ~7u), "owner raw mode");
            stdout.WriteLine(System.Diagnostics.Process.GetCurrentProcess().Id); stdout.Flush();
            if (linger) { System.Threading.Thread.Sleep(60000); return; }
            int command;
            while ((command = stdin.ReadByte()) != -1) {
                if (command == 'c') TestNative.SetConsoleMode(input, mode | 7);
                if (command == 'd') Native.FreeConsole();
            }
            Native.CloseHandle(input);
            Native.FreeConsole();
        }
        public static void ParentHost(string sourcePath) {
            System.Diagnostics.Process child = Owner(sourcePath, true);
            System.Console.WriteLine(Line(child)); System.Console.Out.Flush();
            while (System.Console.OpenStandardInput().ReadByte() != -1) { }
            // Child intentionally outlives this process to test the parent handle.
        }
        public static void Lifecycle(string sourcePath) {
            LifecycleCase(sourcePath, 'e');
            LifecycleCase(sourcePath, 'k');
            LifecycleCase(sourcePath, 'c');
            LifecycleCase(sourcePath, 'd');
            ParentDeath(sourcePath);
        }
        static void LifecycleCase(string sourcePath, char action) {
            System.Diagnostics.Process owner = null, bridge = null;
            try {
                owner = Owner(sourcePath, false);
                uint pid = uint.Parse(Line(owner));
                bridge = Bridge(sourcePath, pid);
                Check(Line(bridge) == Protocol.Ready, "production compiler launch protocol");
                if (action == 'e') {
                    System.Diagnostics.Process duplicate = null;
                    try {
                        duplicate = Bridge(sourcePath, pid);
                        Check(duplicate.WaitForExit(15000), "duplicate helper readiness bounded");
                        Check(duplicate.ExitCode != 0, "duplicate helper must reject same console");
                        Check(duplicate.StandardOutput.ReadToEnd() == "", "duplicate must not announce readiness");
                    } finally { Stop(duplicate); }
                    bridge.StandardInput.Close();
                }
                else if (action == 'k') { owner.Kill(); Check(owner.WaitForExit(5000), "owner death"); }
                else { owner.StandardInput.Write(action); owner.StandardInput.Flush(); }
                Check(bridge.WaitForExit(5000), "helper did not exit after lifetime/raw/member loss: " + action);
                Check(bridge.ExitCode == 0, "helper failed during cleanup: " + action);
                Check(bridge.StandardOutput.ReadToEnd() == "", "helper emitted nonprotocol stdout");
                Check(bridge.StandardError.ReadToEnd() == "", "helper emitted diagnostics/content");
            } finally { Stop(bridge); Stop(owner); }
        }
        static void ParentDeath(string sourcePath) {
            System.Diagnostics.Process parent = null, owner = null, bridge = null;
            try {
                parent = Spawn(CompileCommand(sourcePath, true) + "[Bingo.ImagePaste.ContractTests]::ParentHost('"
                    + sourcePath.Replace("'", "''") + "')");
                uint pid = uint.Parse(Line(parent));
                owner = System.Diagnostics.Process.GetProcessById((int)pid);
                bridge = Bridge(sourcePath, pid);
                Check(Line(bridge) == Protocol.Ready, "parent-death bridge readiness");
                parent.StandardInput.Close();
                Check(parent.WaitForExit(5000), "parent did not exit");
                Check(!owner.HasExited, "owner must survive parent in parent-death test");
                Check(bridge.WaitForExit(5000), "helper did not follow owner parent death");
                Check(bridge.ExitCode == 0, "parent-death helper cleanup");
            } finally { Stop(bridge); Stop(owner); Stop(parent); }
        }
        public static void Crossterm(string executable, string testName) {
            Check(System.IO.File.Exists(executable), "Rust fixture executable missing");
            Check(System.Text.RegularExpressions.Regex.IsMatch(testName, @"\A[A-Za-z0-9_:]+\z"), "Rust fixture name");
            Native.FreeConsole();
            Check(TestNative.AllocConsole(), "allocate crossterm isolated console");
            System.IntPtr input = Native.OpenInput();
            System.Diagnostics.Process fixture = null;
            try {
                Check(Native.ValidHandle(input), "open crossterm isolated input");
                Check(TestNative.FlushConsoleInputBuffer(input), "clear crossterm isolated input");
                System.Diagnostics.ProcessStartInfo info = new System.Diagnostics.ProcessStartInfo();
                info.FileName = executable;
                info.Arguments = "--exact " + testName + " --ignored --nocapture --test-threads=1";
                info.UseShellExecute = false;
                info.CreateNoWindow = false;
                info.RedirectStandardOutput = info.RedirectStandardError = true;
                info.EnvironmentVariables.Remove("BINGO_WINDOWS_IMAGE_PASTE");
                fixture = System.Diagnostics.Process.Start(info);
                System.Diagnostics.Stopwatch budget = System.Diagnostics.Stopwatch.StartNew();
                bool ready = false;
                while (budget.ElapsedMilliseconds < 15000) {
                    System.Threading.Tasks.Task<string> line = fixture.StandardOutput.ReadLineAsync();
                    Check(line.Wait(System.Math.Max(1, 15000 - (int)budget.ElapsedMilliseconds)), "crossterm fixture readiness timeout");
                    if (line.Result == null) break;
                    if (line.Result.Contains("BINGO_CROSSTERM_READING")) { ready = true; break; }
                }
                Check(ready, "Rust crossterm fixture did not announce readiness");
                Check(Native.Raw(input), "Rust crossterm fixture did not enable raw mode");
                Check(Native.Inject(input), "inject production CtrlV into Rust crossterm fixture");
                Check(fixture.WaitForExit(10000), "Rust crossterm fixture did not finish");
                string output = fixture.StandardOutput.ReadToEnd();
                string error = fixture.StandardError.ReadToEnd();
                Check(fixture.ExitCode == 0, "Rust crossterm/Effect::PasteImage fixture failed: " + output + error);
                Check(output.Contains("1 passed"), "Rust crossterm fixture was not executed");
            } finally {
                Stop(fixture);
                if (Native.ValidHandle(input)) Native.CloseHandle(input);
                Native.FreeConsole();
            }
        }
        public static void NativeConsole() {
            Check(System.Runtime.InteropServices.Marshal.SizeOf(typeof(Native.InputRecord)) == 20, "INPUT_RECORD ABI");
            Native.FreeConsole();
            Check(TestNative.AllocConsole(), "allocate isolated console");
            System.IntPtr input = Native.OpenInput();
            try {
                Check(Native.ValidHandle(input), "open isolated CONIN$");
                uint pid = (uint)System.Diagnostics.Process.GetCurrentProcess().Id;
                Check(Native.Contains(pid), "console membership");
                Check(!Native.Contains(0), "reject nonmember");
                Check(Native.Parent(pid) != 0, "parent snapshot");
                uint original;
                Check(Native.GetConsoleMode(input, out original), "read console mode");
                Check(TestNative.SetConsoleMode(input, original | 7), "set cooked console");
                Check(!Native.Raw(input), "cooked input rejected");
                Check(TestNative.SetConsoleMode(input, original & ~7u), "set raw console");
                Check(Native.Raw(input), "raw input accepted");
                Check(TestNative.FlushConsoleInputBuffer(input), "clear isolated input");
                Check(Native.Inject(input), "native WriteConsoleInput failed");
                Native.InputRecord[] records = new Native.InputRecord[2];
                uint read;
                Check(TestNative.ReadConsoleInputW(input, records, 2, out read) && read == 2, "read injected records");
                for (int i = 0; i < 2; i++) {
                    Check(records[i].Type == 1 && records[i].Down == (i == 0 ? 1 : 0), "key down/up layout");
                    Check(records[i].Repeat == 1 && records[i].Key == 0x56, "key repeat/vk layout");
                    Check(records[i].Character == '\x16' && records[i].Modifiers == 8, "CtrlV Unicode/modifiers");
                }
                Check(!Native.Inject(System.IntPtr.Zero), "invalid input must fail open");
                Check(TestNative.SetConsoleMode(input, original), "restore isolated console mode");
            } finally {
                if (Native.ValidHandle(input)) Native.CloseHandle(input);
                Native.FreeConsole();
            }
        }
    }
}
