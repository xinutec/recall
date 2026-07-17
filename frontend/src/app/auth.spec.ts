import { TestBed } from '@angular/core/testing';
import { HttpClient, provideHttpClient, withInterceptors } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';

import { AuthState, authInterceptor } from './auth';

function setup() {
  TestBed.configureTestingModule({
    providers: [
      provideHttpClient(withInterceptors([authInterceptor])),
      provideHttpClientTesting(),
    ],
  });
  return {
    http: TestBed.inject(HttpClient),
    httpMock: TestBed.inject(HttpTestingController),
    auth: TestBed.inject(AuthState),
  };
}

describe('authInterceptor', () => {
  it('raises the sign-in wall on a 401 from an API call', () => {
    const { http, httpMock, auth } = setup();
    expect(auth.needsSignIn()).toBe(false);
    http.get('/api/transcripts').subscribe({ error: () => undefined });
    httpMock.expectOne('/api/transcripts').flush(null, { status: 401, statusText: 'Unauthorized' });
    expect(auth.needsSignIn()).toBe(true);
    httpMock.verify();
  });

  it('leaves the wall down for non-401 errors', () => {
    const { http, httpMock, auth } = setup();
    http.get('/api/transcripts').subscribe({ error: () => undefined });
    httpMock.expectOne('/api/transcripts').flush(null, { status: 500, statusText: 'Server Error' });
    expect(auth.needsSignIn()).toBe(false);
    httpMock.verify();
  });

  it('ignores a 401 that is not an API call', () => {
    const { http, httpMock, auth } = setup();
    http.get('/version').subscribe({ error: () => undefined });
    httpMock.expectOne('/version').flush(null, { status: 401, statusText: 'Unauthorized' });
    expect(auth.needsSignIn()).toBe(false);
    httpMock.verify();
  });
});

describe('AuthState.loginUrl', () => {
  it('returns to the current path and query', () => {
    const auth = new AuthState();
    // jsdom's default location is http://localhost/ — assert the encoded return_to.
    expect(auth.loginUrl()).toBe(`/login?return_to=${encodeURIComponent(location.pathname + location.search)}`);
    expect(auth.loginUrl().startsWith('/login?return_to=')).toBe(true);
  });
});
