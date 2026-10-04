{- Generic list and text helpers of docs/formal's typed catalogue, shared by
   Catalogue.dhall and GitCarry.dhall. No Prelude import: the catalogue
   evaluates offline.
-}

let map =
      \(a : Type) ->
      \(b : Type) ->
      \(f : a -> b) ->
      \(xs : List a) ->
        List/fold a xs (List b) (\(x : a) -> \(acc : List b) -> [ f x ] # acc) ([] : List b)

let concatMap =
      \(a : Type) ->
      \(b : Type) ->
      \(f : a -> List b) ->
      \(xs : List a) ->
        List/fold a xs (List b) (\(x : a) -> \(acc : List b) -> f x # acc) ([] : List b)

let filter =
      \(a : Type) ->
      \(keep : a -> Bool) ->
      \(xs : List a) ->
        List/fold
          a
          xs
          (List a)
          (\(x : a) -> \(acc : List a) -> if keep x then [ x ] # acc else acc)
          ([] : List a)

let any =
      \(a : Type) ->
      \(p : a -> Bool) ->
      \(xs : List a) ->
        List/fold a xs Bool (\(x : a) -> \(acc : Bool) -> p x || acc) False

let all =
      \(a : Type) ->
      \(p : a -> Bool) ->
      \(xs : List a) ->
        List/fold a xs Bool (\(x : a) -> \(acc : Bool) -> p x && acc) True

let natEq =
      \(m : Natural) ->
      \(n : Natural) ->
        Natural/isZero (Natural/subtract m n) && Natural/isZero (Natural/subtract n m)

let isEmpty = \(a : Type) -> \(xs : List a) -> Natural/isZero (List/length a xs)

let join =
      \(sep : Text) ->
      \(xs : List Text) ->
        let Acc = { empty : Bool, text : Text }

        let joined =
              List/fold
                Text
                xs
                Acc
                ( \(x : Text) ->
                  \(acc : Acc) ->
                    if    acc.empty
                    then  { empty = False, text = x }
                    else  { empty = False, text = "${x}${sep}${acc.text}" }
                )
                { empty = True, text = "" }

        in  joined.text

let unlines =
      \(xs : List Text) ->
        List/fold Text xs Text (\(x : Text) -> \(acc : Text) -> "${x}\n${acc}") ""

let showBool = \(b : Bool) -> if b then "TRUE" else "FALSE"

let range =
      \(n : Natural) ->
        Natural/fold
          n
          (List Natural)
          (\(acc : List Natural) -> acc # [ List/length Natural acc ])
          ([] : List Natural)

-- The values of a table, in the order of their positions.
let ordered =
      \(a : Type) ->
      \(index : a -> Natural) ->
      \(xs : List a) ->
        concatMap
          Natural
          a
          (\(i : Natural) -> filter a (\(x : a) -> natEq (index x) i) xs)
          (range (List/length a xs))

in  { map
    , concatMap
    , filter
    , any
    , all
    , natEq
    , isEmpty
    , join
    , unlines
    , showBool
    , range
    , ordered
    }
