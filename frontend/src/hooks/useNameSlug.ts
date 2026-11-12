import { useState } from "react";
import { deriveSlug, resolveSlug } from "../utils/urlRouting";

/** Name + slug form fields: the slug follows the name until the user types into it, then stays theirs (lower-cased). */
export function useNameSlug() {
  const [name, setName] = useState("");
  const [slug, setSlug] = useState("");
  const [slugEdited, setSlugEdited] = useState(false);

  const onNameChange = (val: string) => {
    setName(val);
    if (!slugEdited) {
      setSlug(deriveSlug(val));
    }
  };

  const onSlugChange = (val: string) => {
    setSlug(val.toLowerCase());
    setSlugEdited(true);
  };

  return { name, slug, onNameChange, onSlugChange, finalSlug: () => resolveSlug(name, slug, slugEdited) };
}
