# vlat screenshots

Animated demos of each [vlat](..) view. Start in any view with `-v <view>`, or
press `v` while running to switch views with the view picker.

- [Compact views](#compact-views): [single target](#single-target), [multiple targets](#multiple-targets)
- [Timeline views](#timeline-views): [graph](#graph), [graph with multiple targets](#graph-with-multiple-targets), [EKG](#ekg)
- [Ambient views](#ambient-views): [radar](#radar), [worm](#worm), [bubble](#bubble), [cards](#cards)
- [Interactive dialogs](#interactive-dialogs)

## Compact views

Compact views draw inline, below your prompt, so your terminal history stays
on screen.

### Single target

A live status line for one target: up/down state, current status, and rolling
avg, range, jitter, loss, quality and p95, with a recent-history strip below.

![single target view animated demo](vlat-screen-single.gif)

### Multiple targets

`-v list`: one line per target with a recent-history strip, a range
bar and rolling stats, so you can compare targets at a glance.

![multiple target list view animated demo](vlat-screen-list.gif)

## Timeline views

Fullscreen views that plot latency over time.

### Graph

`-v graph`: a fullscreen latency line graph. With a single target it
adds avg and p95 reference lines.

![single target graph view animated demo](vlat-screen-graph-single.gif)

### Graph with multiple targets

Every target is drawn on one shared chart, with each line labelled in the
graph, for an overall picture of all your targets.

![multiple target graph view animated demo](vlat-screen-graph.gif)

### EKG

`-v ekg`: a hospital-monitor-style trace for each target, stacked in
its own band. Dropped probes show as red `×` marks.

![EKG view animated demo](vlat-screen-ekg.gif)

## Ambient views

Fullscreen views for keeping an eye on things, comparing targets, or impressing
your friends.

### Radar

`-v radar`: a rotating radar sweep. Each target's blip sits farther
from the center the higher its latency. Press `a` to pick a different metric.

![radar view animated demo](vlat-screen-radar.gif)

### Worm

`-v worm`: a retro worm screensaver. Each target is a worm whose
speed and length follow its jitter. Press `a` to pick a different metric.

![worm view animated demo](vlat-screen-worm.gif)

### Bubble

`-v bubble`: floating bubbles, one per target, each sized by that
target's average latency.

![bubble view animated demo](vlat-screen-bubble.gif)

### Cards

`-v cards`: a grid of panels, one per target. Each card shows the
current RTT, a recent-history strip, a range bar and key stats.

![cards view animated demo](vlat-screen-cards.gif)

## Interactive dialogs

The view picker (`v`), theme picker (`t`) and column toggles (`c`). The view
and theme pickers preview your choice live as you move the cursor.

![interactive dialogs animated demo](vlat-screen-dialogs.gif)
