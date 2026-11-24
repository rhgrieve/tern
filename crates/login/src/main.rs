//! The sign-in window. Reads a `SignInRequest` on stdin, shows Microsoft's page, and prints the
//! redirect on stdout once the sign-in is done. It holds no secrets and keeps no cookies.

use std::cell::Cell;
use std::process::ExitCode;
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use tern_auth::SignInRequest;
use webkit6::prelude::*;

const APP_ID: &str = "io.github.tern.Login";
/// Closed without signing in.
const CANCELLED: u8 = 2;

fn main() -> ExitCode {
    let mut line = String::new();
    let request: SignInRequest = match std::io::stdin()
        .read_line(&mut line)
        .map_err(|error| error.to_string())
        .and_then(|_| serde_json::from_str(&line).map_err(|error| error.to_string()))
    {
        Ok(request) => request,
        Err(error) => {
            eprintln!("tern-login: no sign-in request on stdin: {error}");
            return ExitCode::FAILURE;
        }
    };
    let done = Rc::new(Cell::new(false));
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_activate({
        let done = done.clone();
        move |app| show(app, &request, done.clone())
    });
    app.run_with_args::<&str>(&[]);
    if done.get() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(CANCELLED)
    }
}

/// Whether `uri` is the redirect itself, not just an address that starts like it.
fn is_redirect(uri: &str, redirect: &str) -> bool {
    uri.strip_prefix(redirect)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(['?', '#']))
}

fn host_of(uri: &str) -> String {
    glib::Uri::parse(uri, glib::UriFlags::NONE)
        .ok()
        .and_then(|uri| uri.host())
        .map(|host| host.to_string())
        .unwrap_or_default()
}

fn show(app: &adw::Application, request: &SignInRequest, done: Rc<Cell<bool>>) {
    let network = webkit6::NetworkSession::new_ephemeral();
    let view = webkit6::WebView::builder()
        .network_session(&network)
        .vexpand(true)
        .build();
    if let Some(settings) = webkit6::prelude::WebViewExt::settings(&view) {
        settings.set_enable_developer_extras(false);
        settings.set_javascript_can_open_windows_automatically(false);
    }
    // The host stays visible, so a page that is not the organization's sign-in can be noticed.
    let title = adw::WindowTitle::new("Sign in to Teams", "");
    let header = adw::HeaderBar::builder().title_widget(&title).build();
    let frame = adw::ToolbarView::builder().content(&view).build();
    frame.add_top_bar(&header);
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .default_width(520)
        .default_height(720)
        .content(&frame)
        .build();

    view.connect_uri_notify(move |view| {
        title.set_subtitle(&view.uri().map(|uri| host_of(&uri)).unwrap_or_default());
    });
    let redirect = request.redirect.clone();
    let closing = window.downgrade();
    view.connect_decide_policy(move |view, decision, kind| {
        let Some(navigation) = decision.downcast_ref::<webkit6::NavigationPolicyDecision>() else {
            return false;
        };
        let Some(uri) = navigation
            .navigation_action()
            .and_then(|action| action.request())
            .and_then(|request| request.uri())
        else {
            return false;
        };
        if is_redirect(&uri, &redirect) {
            decision.ignore();
            if !done.replace(true) {
                println!("{uri}");
                if let Some(window) = closing.upgrade() {
                    window.close();
                }
            }
            return true;
        }
        if !uri.starts_with("https://") && uri != "about:blank" {
            decision.ignore();
            return true;
        }
        if kind == webkit6::PolicyDecisionType::NewWindowAction {
            decision.ignore();
            view.load_uri(&uri);
            return true;
        }
        false
    });
    view.connect_permission_request(|_, request| {
        request.deny();
        true
    });
    network.connect_download_started(|_, download| download.cancel());
    // Certificate errors must never be clickable away on a sign-in page.
    network.set_tls_errors_policy(webkit6::TLSErrorsPolicy::Fail);
    view.load_uri(&request.url);
    window.present();
}

#[cfg(test)]
mod tests {
    use super::is_redirect;

    #[test]
    fn only_the_exact_redirect_ends_the_sign_in() {
        let redirect = "https://login.example/nativeclient";
        assert!(is_redirect(
            "https://login.example/nativeclient?code=1",
            redirect
        ));
        assert!(!is_redirect(
            "https://login.example/nativeclient.evil/?code=1",
            redirect
        ));
        assert!(!is_redirect(
            "https://other.example/?u=https://login.example/nativeclient",
            redirect
        ));
    }
}
