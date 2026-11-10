# Proves a Poczta test run stays unseen without looking at the screen (started hidden by
# `scripts/win-test.sh probe`, through the task PocztaProbe in the signed-in user's session).
#   win-probe.ps1 <test folder> <max seconds>
# Every pass (a 50 ms sleep plus the enumeration, about 70-90 ms; the WYNIK line gives the measured
# mean, and a flash shorter than that is missed): the foreground window, and the state of every
# top-level window of the test copy (mailclient.exe in <test folder>, and the WebView2 processes
# whose command line names that folder) and of every console window that was not there at the
# start; every 1 s the taskbar buttons named Poczta or like a test window, as placeholders (UI
# Automation, read only). Writes <test folder>\probe.txt (a line per change, the verdict at the
# end; no window titles) and then probe-done.txt.
param([string]$Dir, [int]$MaxSeconds = 120)
$ErrorActionPreference = 'Stop'
Add-Type -ReferencedAssemblies UIAutomationClient, UIAutomationTypes, System.Management -TypeDefinition @'
using System; using System.Collections.Generic; using System.Diagnostics; using System.IO;
using System.Runtime.InteropServices; using System.Text;
public static class PocztaProbe {
  delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32")] static extern bool EnumWindows(EnumProc f, IntPtr l);
  [DllImport("user32")] static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32")] static extern IntPtr GetForegroundWindow();
  [DllImport("user32")] static extern int GetWindowLong(IntPtr h, int i);
  [DllImport("user32")] static extern IntPtr GetWindow(IntPtr h, uint c);
  [DllImport("user32")] static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32")] static extern IntPtr MonitorFromRect(ref RECT r, uint f);
  [DllImport("user32")] static extern bool SetProcessDPIAware();
  [DllImport("user32", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("dwmapi")] static extern int DwmGetWindowAttribute(IntPtr h, int a, out int v, int n);
  [DllImport("user32")] static extern bool GetLayeredWindowAttributes(IntPtr h, out uint key, out byte alpha, out uint flags);
  [DllImport("user32", CharSet = CharSet.Unicode)] static extern IntPtr FindWindow(string cls, string name);
  [StructLayout(LayoutKind.Sequential)] struct CURSORINFO { public int cbSize, flags; public IntPtr hCursor; public int x, y; }
  [DllImport("user32")] static extern bool GetCursorInfo(ref CURSORINFO c);
  [DllImport("user32")] static extern IntPtr LoadCursor(IntPtr inst, IntPtr id);
  // titles of the test copy's windows, without the invisible markers, to know its taskbar buttons by
  static HashSet<string> titles = new HashSet<string>();
  // the markers from their code points: Windows PowerShell reads this BOM-less file as ANSI
  static string Clean(string s) { return s.Replace(((char)0x200B).ToString(), "").Replace(((char)0x200C).ToString(), "").Trim(); }
  // the taskbar's buttons as UI Automation names them (read only: nothing is clicked or shown);
  // only placeholders are written, so no mail subject or other program's title lands in the file
  static string Taskbar() {
    try {
      var bar = System.Windows.Automation.AutomationElement.FromHandle(FindWindow("Shell_TrayWnd", null));
      var all = bar.FindAll(System.Windows.Automation.TreeScope.Descendants, new System.Windows.Automation.PropertyCondition(System.Windows.Automation.AutomationElement.ControlTypeProperty, System.Windows.Automation.ControlType.Button));
      var names = new List<string>();
      foreach (System.Windows.Automation.AutomationElement e in all) {
        string n = Clean(e.Current.Name);
        bool test = false;
        foreach (var t in titles) if (t.Length > 0 && t != "Poczta" && n.IndexOf(t, StringComparison.Ordinal) >= 0) test = true;
        if (test) names.Add("<okno testu>");
        else if (n.IndexOf("Poczta", StringComparison.OrdinalIgnoreCase) >= 0 || n.IndexOf("mailclient", StringComparison.OrdinalIgnoreCase) >= 0) names.Add("<Poczta>");
      }
      names.Sort();
      return string.Join(" | ", names);
    } catch (Exception ex) { return "BLAD UIA: " + ex.Message; }
  }
  public struct RECT { public int L, T, R, B; }
  static Dictionary<uint, string> paths = new Dictionary<uint, string>();
  static Dictionary<uint, bool> tests = new Dictionary<uint, bool>();
  static string PathOf(uint pid) {
    string p;
    if (paths.TryGetValue(pid, out p)) return p;
    try { var pr = Process.GetProcessById((int)pid); try { p = pr.MainModule.FileName; } catch { p = pr.ProcessName; } } catch { p = "?"; }
    return paths[pid] = p;
  }
  // the test copy: its EXE, or a WebView2 process working for it (its data folder is in <dir>)
  static bool IsTest(uint pid, string path, string dir) {
    bool t;
    if (tests.TryGetValue(pid, out t)) return t;
    t = path.StartsWith(dir, StringComparison.OrdinalIgnoreCase) && path.EndsWith("mailclient.exe", StringComparison.OrdinalIgnoreCase);
    if (!t && Path.GetFileName(path).Equals("msedgewebview2.exe", StringComparison.OrdinalIgnoreCase)) {
      try {
        using (var s = new System.Management.ManagementObjectSearcher("SELECT CommandLine FROM Win32_Process WHERE ProcessId=" + pid))
          foreach (System.Management.ManagementObject o in s.Get()) { var c = o["CommandLine"] as string; if (c != null && c.IndexOf(dir, StringComparison.OrdinalIgnoreCase) >= 0) t = true; }
      } catch { }
    }
    return tests[pid] = t;
  }
  static string ClassOf(IntPtr h) { var s = new StringBuilder(256); GetClassName(h, s, 256); return s.ToString(); }
  static string TitleOf(IntPtr h) { var s = new StringBuilder(512); GetWindowText(h, s, 512); return s.ToString(); }
  static bool IsConsole(string cls, string path) {
    string n = Path.GetFileName(path).ToLowerInvariant();
    return cls == "ConsoleWindowClass" || cls == "CASCADIA_HOSTING_WINDOW_CLASS" || n == "cmd.exe" || n == "conhost.exe" || n == "openconsole.exe" || n == "windowsterminal.exe" || n == "powershell.exe";
  }
  public static void Run(string dir, int maxSeconds, string outPath, string readyPath) {
    SetProcessDPIAware();
    var log = new StreamWriter(outPath, false, new UTF8Encoding(false)); log.AutoFlush = true;
    var sw = Stopwatch.StartNew();
    Func<string> t = () => (sw.ElapsedMilliseconds / 1000.0).ToString("F2", System.Globalization.CultureInfo.InvariantCulture);
    var start = new HashSet<IntPtr>();
    EnumWindows((h, l) => { start.Add(h); return true; }, IntPtr.Zero);
    var last = new Dictionary<IntPtr, string>();
    var watched = new HashSet<IntPtr>();
    IntPtr lastFg = (IntPtr)(-1);
    string phase = "przed"; double goneAt = -1; long nextFg = 0;
    int testWindows = 0, testSeen = 0, fgBad = 0, consoleSeen = 0, fgSamples = 0, passes = 0, fgReal = 0;
    bool testUp = false;
    // IDC_APPSTARTING: the arrow with an hourglass Windows shows while a started program gets going
    IntPtr busy = LoadCursor(IntPtr.Zero, (IntPtr)32650); bool wasBusy = false; int busyBefore = 0, busyAfterStart = 0;
    string bar0 = null, lastBar = null; long nextBar = 0; int barChanged = 0, barSamples = 0;
    while (sw.Elapsed.TotalSeconds < maxSeconds) {
      bool anyTest = false;
      var now = new Dictionary<IntPtr, string>();
      EnumWindows((h, l) => {
        uint pid; GetWindowThreadProcessId(h, out pid);
        string path = PathOf(pid), cls = ClassOf(h);
        bool test = IsTest(pid, path, dir);
        bool console = !start.Contains(h) && IsConsole(cls, path);
        if (!test && !console) return true;
        // the app's own EXE decides when the run is over; WebView2 processes may outlive it a moment
        anyTest |= test && path.EndsWith("mailclient.exe", StringComparison.OrdinalIgnoreCase);
        if (test) titles.Add(Clean(TitleOf(h)));
        bool vis = IsWindowVisible(h);
        int cloak; if (DwmGetWindowAttribute(h, 14, out cloak, 4) != 0) cloak = -1;
        RECT r; GetWindowRect(h, out r);
        bool onMon = r.R > r.L && r.B > r.T && MonitorFromRect(ref r, 0) != IntPtr.Zero;
        int ex = GetWindowLong(h, -20);
        IntPtr owner = GetWindow(h, 4);
        bool button = (ex & 0x80) == 0 && ((ex & 0x40000) != 0 || owner == IntPtr.Zero);
        // winit's message window is layered and never given its layered attributes, so Windows never
        // draws it (shortcut: also counts an UpdateLayeredWindow window as unseen; the test copy has none)
        uint key, lf; byte alpha;
        bool clear = (ex & 0x80000) != 0 && (!GetLayeredWindowAttributes(h, out key, out alpha, out lf) || ((lf & 2) != 0 && alpha == 0));
        // a failed cloak query (-1) counts as not cloaked
        bool seen = vis && cloak <= 0 && onMon && !clear;
        if (test && !watched.Contains(h)) { watched.Add(h); testWindows++; }
        if (test && seen) testSeen++;
        if (console && seen) consoleSeen++;
        if (console) watched.Add(h);
        now[h] = string.Format("{0} pid={1} {2} klasa={3} widoczne={4} cloaked={5} rect={6},{7},{8},{9} na_monitorze={10} ex=0x{11:X} owner=0x{12:X} styl_przycisku_paska={13} alfa0={14} WIDAC={15}",
          test ? "TEST" : "KONSOLA", pid, Path.GetFileName(path), cls, vis ? 1 : 0, cloak, r.L, r.T, r.R, r.B, onMon ? 1 : 0, ex, (long)owner, button && vis ? 1 : 0, clear ? 1 : 0, seen ? 1 : 0);
        return true;
      }, IntPtr.Zero);
      foreach (var kv in now) { string o; if (!last.TryGetValue(kv.Key, out o) || o != kv.Value) log.WriteLine(t() + " " + phase + " okno 0x" + ((long)kv.Key).ToString("X") + " " + kv.Value); }
      foreach (var kv in last) if (!now.ContainsKey(kv.Key)) log.WriteLine(t() + " " + phase + " okno 0x" + ((long)kv.Key).ToString("X") + " zamkniete");
      last = now;
      if (anyTest && !testUp) { testUp = true; phase = "w_trakcie"; log.WriteLine(t() + " start kopii testowej"); }
      if (!anyTest && testUp && goneAt < 0) { goneAt = sw.Elapsed.TotalSeconds; phase = "po"; log.WriteLine(t() + " koniec kopii testowej"); }
      IntPtr fg = GetForegroundWindow();
      uint fpid; GetWindowThreadProcessId(fg, out fpid);
      bool bad = watched.Contains(fg) || IsTest(fpid, PathOf(fpid), dir);
      if (bad) fgBad++;
      // a locked session has no foreground window (or the lock screen's): then focus is not tested
      if (phase == "w_trakcie" && fg != IntPtr.Zero && !Path.GetFileName(PathOf(fpid)).Equals("LogonUI.exe", StringComparison.OrdinalIgnoreCase)) fgReal++;
      // a line per change, and one every 500 ms so before/during/after all have samples
      if (fg != lastFg || bad || sw.ElapsedMilliseconds >= nextFg) {
        log.WriteLine(t() + " " + phase + " pierwszy_plan 0x" + ((long)fg).ToString("X") + " " + Path.GetFileName(PathOf(fpid)) + (bad ? " ZLE" : ""));
        lastFg = fg; nextFg = sw.ElapsedMilliseconds + 500; fgSamples++;
      }
      var ci = new CURSORINFO(); ci.cbSize = Marshal.SizeOf(typeof(CURSORINFO));
      bool isBusy = GetCursorInfo(ref ci) && ci.hCursor == busy;
      if (isBusy) { if (phase == "przed") busyBefore++; else busyAfterStart++; }
      if (isBusy != wasBusy) { log.WriteLine(t() + " " + phase + " kursor " + (isBusy ? "APPSTARTING" : "zwykly")); wasBusy = isBusy; }
      // the harness starts the run only after this, so 2 s of samples come before it
      if (readyPath != null && sw.Elapsed.TotalSeconds > 2) { File.WriteAllText(readyPath, "ok"); readyPath = null; }
      if (goneAt >= 0 && sw.Elapsed.TotalSeconds > goneAt + 3) break;
      if (sw.ElapsedMilliseconds >= nextBar) {
        string bar = Taskbar();
        if (bar0 == null) { bar0 = bar; log.WriteLine(t() + " " + phase + " pasek_zadan (przyciski Poczta i okien testu): [" + bar + "]"); }
        else if (bar != lastBar) log.WriteLine(t() + " " + phase + " pasek_zadan (przyciski Poczta i okien testu): [" + bar + "]");
        if (bar != bar0) barChanged++;
        lastBar = bar; nextBar = sw.ElapsedMilliseconds + 1000; barSamples++;
      }
      System.Threading.Thread.Sleep(50);
      passes++;
    }
    bool ok = testWindows > 0 && goneAt >= 0 && testSeen == 0 && fgBad == 0 && consoleSeen == 0 && barChanged == 0 && bar0 != null && !bar0.StartsWith("BLAD");
    // the busy pointer (WebView2 starting its processes) is a visible trace, but not a window
    bool locked = fgReal == 0;
    string verdict = !ok ? "WIDOCZNY LUB NIEDOWIEDZIONY" : "NIEWIDOCZNY" + (locked ? ", ALE SESJA ZABLOKOWANA (pierwszy plan niesprawdzony)" : "") + (busyAfterStart > 0 ? (locked ? ", KURSOR APPSTARTING" : ", ALE KURSOR APPSTARTING") : "");
    log.WriteLine(string.Format("WYNIK: {0} (okna kopii testowej: {1}, probek z widocznym oknem testu: {2}, probek z oknem testu/konsoli na pierwszym planie: {3}, probek z widoczna konsola: {4}, probek pierwszego planu: {5}, przejsc w trakcie z oknem na pierwszym planie (nie ekran blokady): {13}, kopia zakonczona: {6}, probek paska zadan innych niz przed startem: {7} z {8}, probek kursora APPSTARTING przed/od startu zadania: {9}/{10}, przejsc petli: {11}, srednio co {12} ms)",
      verdict, testWindows, testSeen, fgBad, consoleSeen, fgSamples, goneAt >= 0 ? "tak" : "nie", barChanged, barSamples, busyBefore, busyAfterStart, passes, passes > 0 ? sw.ElapsedMilliseconds / passes : 0, fgReal));
    log.Close();
  }
}
'@
'probe.txt', 'probe-done.txt', 'probe-ready.txt' | ForEach-Object { Join-Path $Dir $_ } | Where-Object { Test-Path $_ } | Remove-Item
try { [PocztaProbe]::Run($Dir, $MaxSeconds, (Join-Path $Dir 'probe.txt'), (Join-Path $Dir 'probe-ready.txt')) }
catch { Add-Content (Join-Path $Dir 'probe.txt') ('BLAD SONDY: ' + $_) }
Set-Content (Join-Path $Dir 'probe-done.txt') 'ok'
