import { useState, useEffect, useRef, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { gsap } from "gsap";
import { X, RotateCcw, Square } from "lucide-react";
import { LiquidButton } from "@/components/ui/liquid-glass-button";
import { cn } from "@/lib/utils";

function GlassFilter() {
  return (
    <svg className="hidden">
      <defs>
        <filter
          id="container-glass"
          x="0%"
          y="0%"
          width="100%"
          height="100%"
          colorInterpolationFilters="sRGB"
        >
          <feTurbulence
            type="fractalNoise"
            baseFrequency="0.05 0.05"
            numOctaves="1"
            seed="1"
            result="turbulence"
          />
          <feGaussianBlur in="turbulence" stdDeviation="2" result="blurredNoise" />
          <feDisplacementMap
            in="SourceGraphic"
            in2="blurredNoise"
            scale="70"
            xChannelSelector="R"
            yChannelSelector="B"
            result="displaced"
          />
          <feGaussianBlur in="displaced" stdDeviation="4" result="finalBlur" />
          <feComposite in="finalBlur" in2="finalBlur" operator="over" />
        </filter>
      </defs>
    </svg>
  );
}

type Mode = "idle" | "recording" | "transcribing" | "done";

function clock(total: number): string {
  const mm = Math.floor(total / 60);
  const ss = (total % 60).toString().padStart(2, "0");
  return `${mm}:${ss}`;
}

export default function App() {
  const [mode, setModeState] = useState<Mode>("idle");
  const [seconds, setSeconds] = useState(0);
  const [transcript, setTranscript] = useState("");


  const barRef = useRef<HTMLDivElement>(null);
  const transcriptRef = useRef<HTMLParagraphElement>(null);
  const timerRef = useRef<number | undefined>(undefined);
  const doneTimerRef = useRef<number | undefined>(undefined);
  const finalTextRef = useRef("");
  const modeRef = useRef<Mode>("idle");

  const setMode = useCallback((m: Mode) => {
    modeRef.current = m;
    setModeState(m);
  }, []);

  // Auto-scroll transcript to bottom
  useEffect(() => {
    if (transcriptRef.current) {
      transcriptRef.current.scrollTop = transcriptRef.current.scrollHeight;
    }
  }, [transcript]);

  const clearDoneTimer = useCallback(() => {
    if (doneTimerRef.current) {
      window.clearTimeout(doneTimerRef.current);
      doneTimerRef.current = undefined;
    }
  }, []);

  const dismiss = useCallback(async () => {
    if (timerRef.current) window.clearInterval(timerRef.current);
    clearDoneTimer();
    setMode("idle");
    setTranscript("");
    try {
      await invoke("hide_bar");
    } catch {
      getCurrentWindow().hide();
    }
  }, [clearDoneTimer, setMode]);

  const finish = useCallback(() => {
    setMode("done");
    clearDoneTimer();
    // Brief flash of the final transcript, then auto-dismiss
    doneTimerRef.current = window.setTimeout(() => dismiss(), 2500);
  }, [clearDoneTimer, dismiss, setMode]);

  const spotlightIn = useCallback(() => {
    if (!barRef.current) return;
    gsap.fromTo(
      barRef.current,
      { scale: 0.97, y: 6, filter: "blur(6px)" },
      { scale: 1, y: 0, filter: "blur(0px)", duration: 0.3, ease: "power3.out" }
    );
  }, []);

  const start = useCallback(async () => {
    clearDoneTimer();
    spotlightIn();
    try {
      await invoke("start_recording");
    } catch {
      return;
    }
    finalTextRef.current = "";
    setTranscript("");
    setSeconds(0);
    setMode("recording");
    timerRef.current = window.setInterval(() => setSeconds((s) => s + 1), 1000);
  }, [clearDoneTimer, spotlightIn, setMode]);

  const stop = useCallback(async () => {
    if (timerRef.current) window.clearInterval(timerRef.current);
    setMode("transcribing");

    let text = "";
    try {
      text = await invoke<string>("stop_recording");
    } catch {
      if (!finalTextRef.current) setTranscript("(nothing heard \u2014 try again)");
      finalTextRef.current = finalTextRef.current || "";
      finish();
      return;
    }

    finalTextRef.current = (text ?? "").trim();
    setTranscript(
      finalTextRef.current || "(nothing heard \u2014 try again)"
    );
    invoke("paste_last").catch(() => {});
    finish();
  }, [finish, setMode]);

  const restart = useCallback(async () => {
    try {
      await invoke("cancel_recording");
    } catch {
      /* nothing was recording */
    }
    finalTextRef.current = "";
    setTranscript("");
    start();
  }, [start]);

  const cancelAll = useCallback(async () => {
    try {
      await invoke("cancel_recording");
    } catch {
      /* nothing was recording */
    }
    finalTextRef.current = "";
    dismiss();
  }, [dismiss]);

  // Listen for Tauri events
  useEffect(() => {
    const unlistenPartial = listen<string>("partial", (event) => {
      const m = modeRef.current;
      if (m !== "recording" && m !== "transcribing") return;
      if (event.payload?.length) setTranscript(event.payload);
    });

    const unlistenTrigger = listen("trigger", () => {
      const m = modeRef.current;
      if (m === "idle" || m === "done") start();
      else if (m === "recording") stop();
    });

    return () => {
      unlistenPartial.then((f) => f());
      unlistenTrigger.then((f) => f());
    };
  }, [start, stop]);

  // Keyboard shortcuts: Escape to dismiss, R to stop, RR to restart
  const rTimerRef = useRef<number | undefined>(undefined);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        dismiss();
        return;
      }

      // R key — only while recording
      if ((e.key === "r" || e.key === "R") && modeRef.current === "recording") {
        // Ignore if Ctrl/Shift/Meta held (that's the global shortcut)
        if (e.ctrlKey || e.metaKey) return;

        if (rTimerRef.current) {
          // Second R within window → double-tap → restart
          window.clearTimeout(rTimerRef.current);
          rTimerRef.current = undefined;
          restart();
        } else {
          // First R → wait to see if another comes
          rTimerRef.current = window.setTimeout(() => {
            rTimerRef.current = undefined;
            stop();
          }, 280);
        }
      }
    };
    window.addEventListener("keydown", handler);
    return () => {
      window.removeEventListener("keydown", handler);
      if (rTimerRef.current) window.clearTimeout(rTimerRef.current);
    };
  }, [dismiss, stop, restart]);

  const hasText = transcript.trim().length > 0;
  const isRecording = mode === "recording";
  const isTranscribing = mode === "transcribing";
  const isDone = mode === "done";

  // Equalizer bar delays
  const eqDelays = [0, 0.15, 0.3, 0.1];

  return (
    <div
      ref={barRef}
      className="absolute inset-0 flex items-center select-none"
    >
      {/* ── Liquid glass layers ── */}
      {/* Shadow / edge highlights — same technique as LiquidButton */}
      <div
        className="absolute inset-0 z-0 rounded-[20px]
          shadow-[0_0_6px_rgba(0,0,0,0.03),0_2px_6px_rgba(0,0,0,0.08),inset_3px_3px_0.5px_-3px_rgba(0,0,0,0.9),inset_-3px_-3px_0.5px_-3px_rgba(0,0,0,0.85),inset_1px_1px_1px_-0.5px_rgba(0,0,0,0.6),inset_-1px_-1px_1px_-0.5px_rgba(0,0,0,0.6),inset_0_0_6px_6px_rgba(0,0,0,0.12),inset_0_0_2px_2px_rgba(0,0,0,0.06),0_0_12px_rgba(255,255,255,0.15)]"
      />
      {/* Refraction backdrop filter */}
      <div
        className="absolute inset-0 isolate -z-10 overflow-hidden rounded-[20px]"
        style={{ backdropFilter: 'url("#container-glass")' }}
      />

      {/* ── Content (on top of glass) ── */}
      <div className="relative z-10 flex w-full items-center gap-3 px-8">
        {/* Equalizer */}
        <span className="flex shrink-0 items-center gap-[3px] h-[18px]" aria-hidden="true">
          {eqDelays.map((delay, i) => (
            <i
              key={i}
              className={cn(
                "block w-[3px] rounded-full origin-center",
                isRecording && "bg-[#0a84ff] animate-eq h-full",
                isTranscribing && "bg-black/20 animate-eq-slow h-full",
                isDone && "bg-[#34c759] h-[6px]",
                mode === "idle" && "bg-black/20 h-[6px]"
              )}
              style={{ animationDelay: `${delay}s` }}
            />
          ))}
        </span>

        {/* Center: transcript or status */}
        <div className="flex-1 min-w-0 flex flex-col justify-center">
          {hasText ? (
            <p
              ref={transcriptRef}
              className={cn(
                "text-[14.5px] leading-[1.4] text-black/85 max-h-10 overflow-y-auto pr-0.5",
                "scrollbar-none [&::-webkit-scrollbar]:hidden",
                "[text-shadow:_0_0.5px_1px_rgba(255,255,255,0.3)]",
                (isRecording || isTranscribing) && "after:content-[''] after:inline-block after:w-0.5 after:h-[1em] after:ml-0.5 after:align-[-1px] after:bg-[#0a84ff] after:rounded-sm after:animate-caret"
              )}
            >
              {transcript}
            </p>
          ) : (
            <span className="text-[13px] text-black/40 tracking-[0.1px]">
              {isTranscribing ? "Finalizing\u2026" : "Listening\u2026"}
            </span>
          )}
        </div>

        {/* Timer */}
        {isRecording && (
          <span className="shrink-0 text-xs text-black/40 tabular-nums">
            {clock(seconds)}
          </span>
        )}

        {/* Cancel button — recording only */}
        {isRecording && (
          <button
            onClick={cancelAll}
            className="glass-mini"
            aria-label="Cancel"
          >
            <X size={15} />
          </button>
        )}

        {/* Restart button — recording only */}
        {isRecording && (
          <button
            onClick={restart}
            className="glass-mini"
            aria-label="Restart"
          >
            <RotateCcw size={15} />
          </button>
        )}

        {/* Stop button — recording only */}
        {isRecording && (
          <LiquidButton
            size="default"
            onClick={stop}
            className="shrink-0 text-black/70"
          >
            <Square size={12} fill="currentColor" />
          </LiquidButton>
        )}
      </div>

      {/* SVG glass filter (shared with LiquidButton) */}
      <GlassFilter />
    </div>
  );
}
