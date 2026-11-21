-- waymobar widget: unread mail from the Orion daemon.
--
-- 1. Run the daemon (`orion daemon`, or contrib/orion.service) and make sure
--    `orion` is in PATH (e.g. ln -s ~/orion/target/release/orion ~/.local/bin/).
-- 2. In ~/.config/waymobar/waymobar.hs, add `parse` to the JSON import and
--    `intercalate` to the Data.List one:
--        import Waymobar.JSON (JValue (..), lookupKey, parse)
--        import Data.List (intercalate)
-- 3. Paste the widget below and put it in a bar section, e.g.
--        , right = [ inClickablePill mail, ... ]
-- 4. `waymobar --recompile`, then restart waymobar.
--
-- It reads `orion status --waybar` every 15 s: an envelope while there is
-- nothing new, the unread count (and another colour) when there is, red if
-- an account has an error, and nothing while the daemon is not running.
-- With the pointer on it, the newest unread messages (subject in bold, then
-- sender and account/mailbox), or no popover when there is nothing new; a
-- click opens Orion in foot. `iconAt`, `sym1`, `num` and the colours are the helpers of
-- the sample config (Material Symbols icons); use your own if they differ.

-- | Unread mail from the Orion daemon (`orion daemon`). With the pointer on
-- it, the newest unread messages (subject, sender, mailbox), or no popover
-- at all when there is nothing new. A click opens Orion in foot. Hidden
-- while the daemon is not running.
mail :: Widget
mail = spawnOnClick "foot -e orion" $ periodicHoverIO 15 $ pure $ do
  out <- readCommand "orion status --waybar"
  pure $ case parse out of
    Just o | Just (JStr cls) <- lookupKey "class" o, cls /= "offline" ->
      let s k v = case lookupKey k v of Just (JStr x) -> x; _ -> ""
          arr k = case lookupKey k o of Just (JArr xs) -> xs; _ -> []
          unread = case lookupKey "unread" o of Just (JNum x) -> round x; _ -> 0 :: Int
          msgs = arr "messages"
          message m = "<b>" ++ escape (shorten 60 (s "subject" m)) ++ "</b>\n"
            ++ "<span foreground='" ++ subtext ++ "'>" ++ escape (shorten 40 (s "from" m))
            ++ " · " ++ escape (s "account" m ++ "/" ++ s "mailbox" m) ++ "</span>"
          more = [ "<span foreground='" ++ subtext ++ "'>+" ++ show (unread - length msgs) ++ " more</span>"
                 | not (null msgs), unread > length msgs ]
          errors = [ "<span foreground='" ++ red ++ "'>" ++ escape e ++ "</span>" | JStr e <- arr "errors" ]
          hover = intercalate "\n" (map message msgs ++ more ++ errors)
          n = num (show unread)
      in case cls of
           "error"  -> (iconAt 14 red "mail" ++ "  " ++ n, hover)
           "unread" -> (iconAt 14 peach "mark_email_unread" ++ "  " ++ n, hover)
           _        -> (sym1 "mail", hover)
    _ -> ("", "")
