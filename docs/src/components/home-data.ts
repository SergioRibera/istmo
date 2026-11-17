import rustFirstIcon from '../assets/icons/rust-first.svg?raw';
import nativeApiIcon from '../assets/icons/native-api.svg?raw';
import protocolIcon from '../assets/icons/protocol.svg?raw';
import byoUiIcon from '../assets/icons/byo-ui.svg?raw';
import installIcon from '../assets/icons/install.svg?raw';
import firstAppIcon from '../assets/icons/first-app.svg?raw';
import firstPluginIcon from '../assets/icons/first-plugin.svg?raw';
import conceptsIcon from '../assets/icons/concepts.svg?raw';

export const whyIstmo = [
  {
    icon: rustFirstIcon,
    title: 'Rust first, native second',
    body: 'Your business logic, state and glue live in a single Rust crate. Kotlin and Swift show up only where the platform demands them.',
  },
  {
    icon: nativeApiIcon,
    title: 'Real native APIs',
    body: 'Sign in with Google, publish Live Activities, request background permissions. Plugins call platform SDKs directly, not through a lowest-common-denominator layer.',
  },
  {
    icon: protocolIcon,
    title: 'One binary protocol',
    body: 'One versioned frame protocol carries every call. JNI and FFI only see wincode payload bytes. The Kotlin and Swift glue is generated source you can read.',
  },
  {
    icon: byoUiIcon,
    title: 'Bring your own UI',
    body: 'istmo has no opinion on rendering. Ship eframe, winit, slint, SwiftUI, Jetpack Compose, or a native Activity. The framework stays underneath.',
  },
];

export const startHere = [
  {
    icon: installIcon,
    title: 'Install istmo',
    body: 'Add the framework to a new or existing Rust workspace. One cargo add, one build.rs line.',
  },
  {
    icon: firstAppIcon,
    title: 'Build your first app',
    body: 'Scaffold a full-Rust mobile app from the template repo. Android and iOS projects included.',
  },
  {
    icon: firstPluginIcon,
    title: 'Write your first plugin',
    body: 'Wrap a native SDK behind a Rust trait. Call it from Rust, Kotlin and Swift with typed clients.',
  },
  {
    icon: conceptsIcon,
    title: 'Read the concepts',
    body: 'Contracts, host versus client, manifests, the frame protocol. The flat map of the architecture.',
  },
];
