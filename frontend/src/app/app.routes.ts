import { Routes } from '@angular/router';

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
  { path: 'review', title: 'recall · review', component: Review },
  { path: 'train', title: 'recall · train', component: Train },
  { path: 'labels', title: 'recall · review labels', component: Labels },
  { path: 'sessions', title: 'recall · sessions', component: Sessions },
  { path: 'sessions/:id', title: 'recall · session', component: Session },
  { path: '**', redirectTo: '' },
];
