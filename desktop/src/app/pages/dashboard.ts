import { Component, inject, output } from '@angular/core';
import { DatePipe } from '@angular/common';
import { Store, fmt } from '../store';
import { DeviceLive, DeviceStatus } from '../models';

@Component({
  selector: 'sj-dashboard',
  imports: [DatePipe],
  template: `
    <header class="page-header">
      <div>
        <h1>Dashboard</h1>
        <p class="muted">Skipjack watches your TVs and mutes or skips YouTube ads.</p>
      </div>
      @if (store.running()) {
        <button class="btn danger" (click)="store.stop()" [disabled]="store.busy()">■ Stop</button>
      } @else {
        <button class="btn primary big" (click)="store.start()" [disabled]="store.busy() || !hasDevices()">
          ▶ Start
        </button>
      }
    </header>

    @if (!hasDevices()) {
      <section class="card empty">
        <h2>No TV linked yet</h2>
        <p class="muted">Pair a TV with a “Link with TV code” or scan your network to get started.</p>
        <button class="btn primary" (click)="navigate.emit('devices')">Add a device</button>
      </section>
    }

    <section class="stats">
      <div class="stat">
        <div class="value">{{ store.adsSkipped() }}</div>
        <div class="label">ads skipped</div>
      </div>
      <div class="stat">
        <div class="value">{{ store.adsMuted() }}</div>
        <div class="label">ads muted</div>
      </div>
      <div class="stat">
        <div class="value">{{ connectedCount() }}/{{ store.deviceList().length }}</div>
        <div class="label">devices connected</div>
      </div>
    </section>
    @if (store.adsMuted() || store.adsSkipped()) {
      <p class="small muted">
        Totals since first run ·
        <a href="#" (click)="$event.preventDefault(); store.resetStats()">reset</a>
      </p>
    }

    <section class="device-grid">
      @for (d of store.deviceList(); track d.screen_id) {
        <article class="card device">
          <div class="device-head">
            <span class="status-dot" [attr.data-kind]="d.status.kind"></span>
            <div class="grow">
              <div class="title">{{ d.name }}</div>
              <div class="muted small">{{ statusText(d.status) }}</div>
            </div>
            @if (d.ads) {
              <span class="badge">{{ d.ads }} ads</span>
            }
          </div>
          @if (d.status.kind === 'connected') {
            @if (d.playback?.video_id) {
              <div class="now-playing">
                <img [src]="thumb(d)" alt="" loading="lazy" />
                <div>
                  <div class="small muted">{{ playerText(d) }}</div>
                  <div class="mono small">{{ d.playback!.video_id }}</div>
                </div>
              </div>
            } @else {
              <p class="small muted">Nothing playing on the TV right now.</p>
            }
            <div class="remote">
              <div class="remote-row">
                <button class="btn ghost" (click)="store.remotePrevious(d.screen_id)" title="Previous">⏮</button>
                @if (d.playback?.state === 'playing') {
                  <button class="btn ghost" (click)="store.remotePause(d.screen_id)" title="Pause">❚❚</button>
                } @else {
                  <button class="btn ghost" (click)="store.remotePlay(d.screen_id)" title="Play">▶</button>
                }
                <button class="btn ghost" (click)="store.remoteNext(d.screen_id)" title="Next">⏭</button>
                <button class="btn ghost" (click)="store.remoteSeek(d.screen_id, seekPos(d, -10))" title="Back 10s">−10s</button>
                <button class="btn ghost" (click)="store.remoteSeek(d.screen_id, seekPos(d, 10))" title="Forward 10s">+10s</button>
              </div>
              <div class="remote-row">
                <button class="btn ghost" (click)="store.remoteDpad(d.screen_id, 'LEFT')" title="Left">◀</button>
                <button class="btn ghost" (click)="store.remoteDpad(d.screen_id, 'UP')" title="Up">▲</button>
                <button class="btn ghost" (click)="store.remoteDpad(d.screen_id, 'DOWN')" title="Down">▼</button>
                <button class="btn ghost" (click)="store.remoteDpad(d.screen_id, 'RIGHT')" title="Right">▶</button>
                <button class="btn ghost" (click)="store.remoteDpad(d.screen_id, 'ENTER')" title="OK">OK</button>
                <button class="btn ghost" (click)="store.remoteDpad(d.screen_id, 'BACK')" title="Back">↩</button>
                <select class="speed" [value]="speedOf(d)" (change)="store.remoteSpeed(d.screen_id, +$any($event.target).value)" title="Playback speed">
                  @for (s of speeds; track s) {
                    <option [value]="s">{{ s }}×</option>
                  }
                </select>
              </div>
              <div class="remote-row queue-row">
                <input #qv class="queue-input" placeholder="video id or URL" title="Video id or YouTube URL" />
                <button class="btn ghost" (click)="playVideo(d.screen_id, qv.value); qv.value = ''" title="Play now (interrupts playback)">Play</button>
                <button class="btn ghost" (click)="store.remoteAddVideo(d.screen_id, videoId(qv.value))" title="Add to end of queue">+End</button>
                <button class="btn ghost" (click)="store.remoteInsertVideo(d.screen_id, videoId(qv.value))" title="Play next">+Next</button>
                <button class="btn ghost" (click)="store.remoteRemoveVideo(d.screen_id, videoId(qv.value))" title="Remove from queue">−</button>
                <button class="btn ghost danger-text" (click)="clearQueue(d.screen_id)" title="Clear queue">Clear</button>
              </div>
            </div>
            @if (d.queue || d.volume || d.quality) {
              <div class="device-state small muted">
                @if (d.queue?.video_ids?.length) {
                  <span title="Queue (from playlistModified)">⧉ {{ d.queue!.video_ids.length }} in queue</span>
                }
                @if (d.volume) {
                  <span title="Volume (from onVolumeChanged)">🔊 {{ d.volume.volume }}{{ d.volume.muted ? ' muted' : '' }}</span>
                }
                @if (d.quality?.level) {
                  <span title="Quality (from onVideoQualityChanged)">{{ d.quality!.level }}p</span>
                }
              </div>
            }
          }
        </article>
      }
    </section>

    <section class="card">
      <h2>Activity</h2>
      @if (store.activity().length === 0) {
        <p class="muted">Nothing happened yet. Start the service and play a video on your TV.</p>
      }
      <ul class="activity">
        @for (a of store.activity(); track $index) {
          <li [attr.data-level]="a.level">
            <span class="time">{{ a.at | date: 'HH:mm:ss' }}</span>
            @if (a.device) {
              <span class="who">{{ a.device }}</span>
            }
            <span>{{ a.text }}</span>
          </li>
        }
      </ul>
      @if (store.meta()?.log_path; as log) {
        <p class="small muted">Detailed log: <span class="mono">{{ log }}</span></p>
      }
    </section>
  `,
})
export class DashboardPage {
  protected readonly store = inject(Store);
  readonly navigate = output<string>();
  protected readonly speeds = [0.5, 0.75, 1, 1.25, 1.5, 1.75, 2];

  protected speedOf(d: DeviceLive): number {
    return 1;
  }

  protected seekPos(d: DeviceLive, delta: number): number {
    return Math.max(0, (d.playback?.current_time ?? 0) + delta);
  }

  /** Accepts a bare id or a full YouTube URL, returns the 11-char id. */
  protected videoId(input: string): string {
    const s = input.trim();
    const m = s.match(/(?:v=|youtu\.be\/|shorts\/|embed\/)([\w-]{11})/);
    return m ? m[1] : s;
  }

  /** Play interrupts playback — confirm first, per PLAN-remote-control. */
  protected playVideo(screenId: string, input: string): void {
    const id = this.videoId(input);
    if (!id) return;
    if (confirm('Play this video now? It will interrupt current playback.')) {
      void this.store.remotePlayVideo(screenId, id);
    }
  }

  protected clearQueue(screenId: string): void {
    if (confirm('Clear the queue on this TV?')) {
      void this.store.remoteClearPlaylist(screenId);
    }
  }

  protected hasDevices(): boolean {
    return (this.store.draft()?.devices.length ?? 0) > 0;
  }

  protected connectedCount(): number {
    return this.store.deviceList().filter((d) => d.status.kind === 'connected').length;
  }

  protected thumb(d: DeviceLive): string {
    return `https://img.youtube.com/vi/${d.playback!.video_id}/mqdefault.jpg`;
  }

  protected statusText(s: DeviceStatus): string {
    switch (s.kind) {
      case 'authenticating':
        return 'Authenticating…';
      case 'waiting_for_device':
        return 'Waiting for the TV to come online';
      case 'connecting':
        return 'Connecting…';
      case 'connected':
        return s.screen_name ? `Connected to ${s.screen_name}` : 'Connected';
      case 'error':
        return s.message;
      default:
        return this.store.running() ? 'Reconnecting…' : 'Idle';
    }
  }

  protected playerText(d: DeviceLive): string {
    const p = d.playback!;
    const labels: Record<string, string> = {
      playing: '▶ Playing',
      paused: '❚❚ Paused',
      advertisement: '⚠ Advertisement',
      starting: '… Buffering',
      buffering: 'Ended',
      cued: 'Cued',
      stopped: 'Stopped',
      ad_skipped: 'Ad skipped',
    };
    const pos = p.duration ? ` · ${fmt(p.current_time)} / ${fmt(p.duration)}` : '';
    return `${labels[p.state] ?? p.state}${pos}`;
  }
}
