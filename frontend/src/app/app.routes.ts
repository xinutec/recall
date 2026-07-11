import { Routes } from '@angular/router';

import { Ask } from './features/ask';
import { Cleanup } from './features/cleanup';
import { Compare } from './features/compare';
import { CompareRun } from './features/compare-run';
import { Labels } from './features/labels';
import { Review } from './features/review';
import { Search } from './features/search';
import { Session } from './features/session';
import { Sessions } from './features/sessions';
import { Timeline } from './features/timeline';
import { Train } from './features/train';

export const routes: Routes = [
  { path: '', pathMatch: 'full', title: 'recall · timeline', component: Timeline },
  { path: 'search', title: 'recall · search', component: Search },
  { path: 'ask', title: 'recall · ask', component: Ask },
  { path: 'review', title: 'recall · review', component: Review },
  { path: 'train', title: 'recall · train', component: Train },
  { path: 'labels', title: 'recall · review labels', component: Labels },
  { path: 'cleanup', title: 'recall · cleanup', component: Cleanup },
  { path: 'sessions', title: 'recall · sessions', component: Sessions },
  { path: 'compare', title: 'recall · compare', component: Compare },
  { path: 'compare/:id', title: 'recall · comparison', component: CompareRun },
  { path: 'sessions/:id', title: 'recall · session', component: Session },
  { path: '**', redirectTo: '' },
];
