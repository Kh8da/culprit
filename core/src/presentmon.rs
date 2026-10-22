//! Reads the CSV that Intel's PresentMon prints, one row per frame.
//!
//! Columns are found by name from the header, because they move between PresentMon
//! versions and options. The names here match PresentMon 2.x run with `--qpc_time`.

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RowTime {
    /// A QueryPerformanceCounter value, comparable with the recorder's own clock.
    Qpc(u64),
    /// Milliseconds since PresentMon started recording.
    Ms(f64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PresentRow {
    pub app: String,
    pub pid: u32,
    pub time: RowTime,
    pub ms_between_presents: f32,
    pub cpu_busy: Option<f32>,
    pub gpu_busy: Option<f32>,
}

#[derive(Clone, Copy, Debug)]
enum TimeCol {
    Qpc(usize),
    Seconds(usize),
    Ms(usize),
}

#[derive(Clone, Debug)]
pub struct CsvLayout {
    columns: usize,
    app: usize,
    pid: usize,
    time: TimeCol,
    between_presents: usize,
    cpu_busy: Option<usize>,
    gpu_busy: Option<usize>,
}

impl CsvLayout {
    /// Reads the header line. Fails if the columns Culprit needs aren't there.
    pub fn from_header(line: &str) -> Result<Self, String> {
        let names: Vec<&str> = line.trim_end_matches(['\r', '\n']).split(',').map(str::trim).collect();
        let find = |wanted: &[&str]| wanted.iter().find_map(|w| names.iter().position(|n| n.eq_ignore_ascii_case(w)));
        let app = find(&["Application"]).ok_or("PresentMon output has no Application column")?;
        let pid = find(&["ProcessID"]).ok_or("PresentMon output has no ProcessID column")?;
        let between_presents =
            find(&["MsBetweenPresents", "FrameTime", "MsBetweenAppStart"]).ok_or("PresentMon output has no frame time column")?;
        let time = if let Some(i) = find(&["TimeInQPC", "CPUStartQPC"]) {
            TimeCol::Qpc(i)
        } else if let Some(i) = find(&["TimeInSeconds", "CPUStartTimeInSeconds"]) {
            TimeCol::Seconds(i)
        } else if let Some(i) = find(&["TimeInMs", "CPUStartTimeInMs", "CPUStartTime"]) {
            TimeCol::Ms(i)
        } else {
            return Err("PresentMon output has no time column".into());
        };
        Ok(Self {
            columns: names.len(),
            app,
            pid,
            time,
            between_presents,
            cpu_busy: find(&["MsCPUBusy", "CPUBusy"]),
            gpu_busy: find(&["MsGPUBusy", "GPUBusy"]),
        })
    }

    /// Parses one data row. Returns `None` for anything that isn't a complete frame row.
    pub fn parse(&self, line: &str) -> Option<PresentRow> {
        let line = line.trim_end_matches(['\r', '\n']);
        let mut fields: Vec<&str> = line.split(',').collect();
        // An exe name with commas in it spills into extra columns; fold them back in.
        let extra = fields.len().checked_sub(self.columns)?;
        let app = if extra > 0 && self.app == 0 {
            let joined = fields[..=extra].join(",");
            fields.drain(..extra);
            joined
        } else if extra == 0 {
            fields[self.app].to_string()
        } else {
            return None;
        };
        let num = |i: usize| fields.get(i).and_then(|s| s.trim().parse::<f64>().ok()).filter(|v| v.is_finite());
        let time = match self.time {
            TimeCol::Qpc(i) => RowTime::Qpc(fields.get(i)?.trim().parse().ok()?),
            TimeCol::Seconds(i) => RowTime::Ms(num(i)? * 1000.0),
            TimeCol::Ms(i) => RowTime::Ms(num(i)?),
        };
        let ms = num(self.between_presents)? as f32;
        if ms < 0.0 {
            return None;
        }
        Some(PresentRow {
            app,
            pid: fields.get(self.pid)?.trim().parse().ok()?,
            time,
            ms_between_presents: ms,
            cpu_busy: self.cpu_busy.and_then(num).map(|v| v as f32),
            gpu_busy: self.gpu_busy.and_then(num).map(|v| v as f32),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Header PresentMon 2.6 prints with --qpc_time and default metrics.
    const HEADER: &str = "Application,ProcessID,SwapChainAddress,PresentRuntime,SyncInterval,PresentFlags,AllowsTearing,PresentMode,TimeInQPC,MsBetweenSimulationStart,MsBetweenPresents,MsBetweenDisplayChange,MsInPresentAPI,MsRenderPresentLatency,MsUntilDisplayed,CPUStartQPC,MsBetweenAppStart,MsCPUBusy,MsCPUWait,MsGPULatency,MsGPUTime,MsGPUBusy,MsGPUWait,MsAnimationError,AnimationTime,MsFlipDelay,MsAllInputToPhotonLatency,MsClickToPhotonLatency";

    #[test]
    fn parses_presentmon_rows() {
        let layout = CsvLayout::from_header(HEADER).unwrap();
        let row = "cs2.exe,4120,0x000001D2A8E4C0A0,DXGI,0,512,1,Hardware: Independent Flip,123456789012,NA,6.9443,6.9400,0.1021,4.2000,5.5000,123456700000,6.9500,5.1000,1.8443,0.5000,4.0000,3.9000,0.1000,NA,NA,0.0000,NA,NA\r\n";
        let r = layout.parse(row).unwrap();
        assert_eq!(r.app, "cs2.exe");
        assert_eq!(r.pid, 4120);
        assert_eq!(r.time, RowTime::Qpc(123456789012));
        assert!((r.ms_between_presents - 6.9443).abs() < 1e-4);
        assert_eq!(r.cpu_busy, Some(5.1));
        assert_eq!(r.gpu_busy, Some(3.9));
    }

    #[test]
    fn handles_commas_in_names_and_junk() {
        let layout = CsvLayout::from_header(HEADER).unwrap();
        let row = "Hello, World.exe,77,0x0,DXGI,1,0,0,Composed: Flip,1000,NA,16.6,16.6,0.1,1,1,999,16.6,2,14,1,2,2,0,NA,NA,0,NA,NA";
        let r = layout.parse(row).unwrap();
        assert_eq!(r.app, "Hello, World.exe");
        assert_eq!(r.pid, 77);
        assert!(layout.parse("").is_none());
        assert!(layout.parse("Stopped recording.").is_none());
        let na_frame = "game.exe,1,0x0,DXGI,1,0,0,Composed: Flip,1000,NA,NA,16.6,0.1,1,1,999,16.6,2,14,1,2,2,0,NA,NA,0,NA,NA";
        assert!(layout.parse(na_frame).is_none());
    }

    #[test]
    fn falls_back_to_relative_seconds() {
        let layout = CsvLayout::from_header("Application,ProcessID,TimeInSeconds,MsBetweenPresents").unwrap();
        let r = layout.parse("game.exe,9,1.5,16.7").unwrap();
        assert_eq!(r.time, RowTime::Ms(1500.0));
        assert_eq!(r.cpu_busy, None);
        assert!(CsvLayout::from_header("Application,ProcessID").is_err());
    }
}
