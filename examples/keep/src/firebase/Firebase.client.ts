import { initializeApp } from "firebase/app";
import { getAnalytics } from "firebase/analytics";
import type { User } from "firebase/auth";
import {
  getAuth,
  GoogleAuthProvider,
  onAuthStateChanged,
  signInWithPopup,
  signOut as firebaseSignOut,
} from "firebase/auth";
import type { FirebaseAuthUser } from "./bindings";

type AuthStateCallback = (user: FirebaseAuthUser | null) => void;
type Unsubscribe = () => void;

const firebaseConfig = {
  apiKey: "AIzaSyBUejC7o9O-Az6T0FiP8HGpvj9znDwx0hI",
  authDomain: "pocoknote.firebaseapp.com",
  projectId: "pocoknote",
  storageBucket: "pocoknote.firebasestorage.app",
  messagingSenderId: "769761039895",
  appId: "1:769761039895:web:a23d7df92aafa7a2106e52",
  measurementId: "G-Y8G5SEHX8W",
};

const app = initializeApp(firebaseConfig);
const analytics = getAnalytics(app);
const auth = getAuth(app);
const provider = new GoogleAuthProvider();

provider.setCustomParameters({ prompt: "select_account" });

async function userPayload(user: User | null): Promise<FirebaseAuthUser | null> {
  if (!user) {
    return null;
  }

  return {
    token: await user.getIdToken(),
    uid: user.uid,
    email: user.email,
    name: user.displayName,
    photoUrl: user.photoURL,
  };
}

export default {
  async signIn(): Promise<FirebaseAuthUser | null> {
    const credential = await signInWithPopup(auth, provider);
    return userPayload(credential.user);
  },

  async signOut(): Promise<FirebaseAuthUser | null> {
    await firebaseSignOut(auth);
    return null;
  },

  async currentUser(): Promise<FirebaseAuthUser | null> {
    return userPayload(auth.currentUser);
  },

  async initialUser(): Promise<FirebaseAuthUser | null> {
    await auth.authStateReady();
    return userPayload(auth.currentUser);
  },

  onAuthStateChanged(callback: AuthStateCallback): Unsubscribe {
    return onAuthStateChanged(auth, async (user) => {
      try {
        callback(await userPayload(user));
      } catch (error) {
        console.error("Firebase auth state bridge failed", error);
        callback(null);
      }
    });
  },
};

void analytics;
